//! The interactive score (music for a game; see
//! [`faderframe_project::interactive`]): its edits, a preview that plays
//! its rules on the engine, and its export for game engines.
//!
//! Edits ([`AdaptiveOp`]) are one undo step each (a
//! [`Command::SetInteractiveScore`] in a batch named after the edit); the
//! session allocates every id.
//!
//! **The preview** (not saved) plays one state at a time and moves between
//! them as the game would: a section state is the transport looping its
//! loop (the loop held by the session, not the project's —
//! `EngineController::hold_loop`), a scene state the launcher playing its
//! row. Asked for another state, the session finds the rule
//! ([`InteractiveScore::rule_between`]) and its sync point in the state
//! playing ([`sync_point`]) far enough ahead that the render-ahead tiers
//! have not rendered past it yet, and plans the transition there: a jump
//! of the transport on that exact frame (`EngineController::set_cues`;
//! with a transition segment, two jumps), a launch of a scene's clips or a
//! return to the arrangement on that sample (`Quantize::At`,
//! `LaunchCommand::Arrangement`), and the rule's fades on the outputs
//! (`EngineController::transition_fade`). The tick follows the transport's
//! cues taken and plans again when a jump went missing (a locate in
//! between). Parameters move the layers' tracks with gains beside their
//! faders (`EngineController::set_track_trims`), never in the project.
//! Stingers are launcher clips launched on their sync point and released
//! a clip's length later. A state that plays once goes on to its `then`
//! at its end, or the transport stops there.
//!
//! **Export** ([`export`]): stems per state and layer with their loop
//! points and cues in the files, a JSON manifest, an FMOD Studio script,
//! a Wwise Audio File Importer list and a WAAPI script ([`formats`]).

pub mod export;
pub mod formats;

use crate::{NoticeLevel, Result, Session, SessionError, TransportAction};
use faderframe_core::{MusicLayerId, MusicStateId, ScoreParamId, StingerId, TrackId, TransitionId};
use faderframe_engine::launch::{LaunchCommand, Quantize};
use faderframe_engine::transition::TransitionFade;
use faderframe_project::Command;
use faderframe_project::interactive::{
    InteractiveScore, LayerCurve, MusicLayer, MusicState, Placement, PlacementKind, RuleEnd,
    ScoreExport, ScoreParameter, StateSource, Stinger, SyncContext, SyncPoint, TransitionRule,
    sync_point,
};
use faderframe_project::launcher::SlotKey;
use faderframe_transport::{Cue, Cues, LoopRange};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// An edit of the score, a preview control, an export.
#[derive(Clone, Debug, PartialEq)]
pub enum AdaptiveOp {
    // --- edits (one undo step each) ---
    /// A state playing a section or a launcher scene (named after it).
    AddState(StateSource),
    /// A state for every section not yet one (in project order).
    StatesFromSections,
    /// A state for every launcher scene not yet one.
    StatesFromScenes,
    /// A state as given (by id).
    SetState(MusicState),
    RemoveState(MusicStateId),
    /// A state to place `to` in the list.
    MoveState {
        state: MusicStateId,
        to: usize,
    },
    /// A layer of `tracks` (empty: the selected tracks), named or numbered.
    AddLayer {
        name: Option<String>,
        tracks: Vec<TrackId>,
    },
    SetLayer(MusicLayer),
    RemoveLayer(MusicLayerId),
    /// A rule from `from` to `to` (next bar, from the start).
    AddRule {
        from: RuleEnd,
        to: RuleEnd,
    },
    SetRule(TransitionRule),
    RemoveRule(TransitionId),
    /// A rule to place `to` among the rules (precedence among equals).
    MoveRule {
        rule: TransitionId,
        to: usize,
    },
    /// A parameter (0…1), named or "Intensity".
    AddParameter {
        name: Option<String>,
    },
    SetParameter(ScoreParameter),
    RemoveParameter(ScoreParamId),
    /// A stinger playing a launcher slot's clip (on the next beat).
    AddStinger {
        slot: SlotKey,
    },
    SetStinger(Stinger),
    RemoveStinger(StingerId),
    /// The state the music starts in.
    SetStart(Option<MusicStateId>),
    SetExport(ScoreExport),
    // --- the preview (not undoable) ---
    /// Start playing the score (from its first state) or stop.
    Preview(bool),
    /// Go to a state (the preview starts there when it is not running).
    Go(MusicStateId),
    /// Play a stinger on its sync point.
    PlayStinger(StingerId),
    /// A parameter's value in the preview.
    SetValue {
        parameter: ScoreParamId,
        value: f32,
    },
    // --- export ---
    /// Export stems and metadata into a folder (in the background).
    Export(PathBuf),
    CancelExport,
}

impl AdaptiveOp {
    /// Whether it edits the project (an undo step).
    pub fn edits(&self) -> bool {
        !matches!(
            self,
            AdaptiveOp::Preview(_)
                | AdaptiveOp::Go(_)
                | AdaptiveOp::PlayStinger(_)
                | AdaptiveOp::SetValue { .. }
                | AdaptiveOp::Export(_)
                | AdaptiveOp::CancelExport
        )
    }
}

/// A transition planned in the preview.
#[derive(Clone, Debug, PartialEq)]
struct Planned {
    to: MusicStateId,
    /// A transition segment played on the way.
    via: Option<MusicStateId>,
    /// The sync point (engine samples).
    at: i64,
    /// Planned jumps of the transport (0: a launch only) and the cues taken
    /// when they were handed over.
    jumps: u32,
    taken: u32,
    /// What the destination's placement will be once there.
    arrival: Arrival,
    /// When it was planned (a jump that never comes is planned again).
    when: Instant,
}

/// How a destination plays once the transition is done.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Arrival {
    /// A section: where it is.
    Section,
    /// A scene launched on this sample.
    Scene { start: i64 },
}

/// The preview as it runs.
#[derive(Debug)]
struct Preview {
    /// The state playing (as processed).
    current: Option<MusicStateId>,
    /// A scene state's launch sample (its clips' start).
    origin: i64,
    /// Playing a transition segment now.
    segment: Option<MusicStateId>,
    planned: Option<Planned>,
    /// The loop held for the state playing.
    held: Option<(Option<LoopRange>, bool)>,
    /// A one-shot state's end has been dealt with.
    ended: bool,
}

/// Interactive score work in the session (not saved).
#[derive(Default)]
pub struct AdaptiveState {
    preview: Option<Preview>,
    /// The parameters' values in the preview.
    values: BTreeMap<ScoreParamId, f32>,
    export: Option<export::ExportJob>,
    last_export: Option<std::result::Result<export::ExportReport, String>>,
}

/// What the view shows of the preview and the export.
#[derive(Clone, Debug, PartialEq)]
pub struct AdaptiveStatus {
    pub previewing: bool,
    /// The state playing.
    pub current: Option<MusicStateId>,
    /// A transition segment playing on the way to another state.
    pub segment: Option<MusicStateId>,
    /// A transition waiting: to which state, in how many seconds (as
    /// processed), on which sync point.
    pub queued: Option<(MusicStateId, f64, String)>,
    /// Where the waiting transition happens (engine samples, the timeline
    /// position of its sync point).
    pub queued_at: Option<i64>,
    /// An export running: how far.
    pub exporting: Option<f64>,
    /// The last export: its folder and files, or what went wrong.
    pub last_export: Option<std::result::Result<export::ExportReport, String>>,
}

impl Session {
    // --- reading -------------------------------------------------------

    /// The interactive score.
    pub fn interactive(&self) -> &InteractiveScore {
        &self.project.interactive
    }

    /// A parameter's value in the preview (its default until moved).
    pub fn score_value(&self, parameter: ScoreParamId) -> f32 {
        let Some(p) = self.project.interactive.parameter(parameter) else {
            return 0.0;
        };
        p.clamp(
            self.adaptive
                .values
                .get(&parameter)
                .copied()
                .unwrap_or(p.default),
        )
    }

    /// The preview's and the export's state.
    pub fn adaptive_status(&self) -> AdaptiveStatus {
        let rate = f64::from(self.engine.sample_rate().max(1));
        let pos = self.processing_position();
        let preview = self.adaptive.preview.as_ref();
        AdaptiveStatus {
            previewing: preview.is_some(),
            current: preview.and_then(|p| p.current),
            segment: preview.and_then(|p| p.segment),
            queued: preview.and_then(|p| p.planned.as_ref()).map(|q| {
                let left = match q.jumps {
                    // Ahead in playback, not on the timeline (a loop may
                    // wrap first): an estimate from the loop.
                    _ if q.at >= pos => (q.at - pos) as f64 / rate,
                    _ => preview
                        .and_then(|p| p.held)
                        .and_then(|(r, on)| r.filter(|_| on))
                        .map_or(0.0, |r| {
                            ((r.end - pos) + (q.at - r.start)).max(0) as f64 / rate
                        }),
                };
                let rule = self
                    .project
                    .interactive
                    .rule_between(preview.and_then(|p| p.current), q.to);
                (q.to, left, rule.sync.label())
            }),
            queued_at: preview.and_then(|p| p.planned.as_ref()).map(|q| q.at),
            exporting: self.adaptive.export.as_ref().map(|j| j.fraction()),
            last_export: self.adaptive.last_export.clone(),
        }
    }

    /// Whether the preview runs (for repaints).
    pub fn adaptive_previewing(&self) -> bool {
        self.adaptive.preview.is_some()
    }

    // --- actions -------------------------------------------------------

    pub(crate) fn adaptive_op(&mut self, op: AdaptiveOp) -> Result<()> {
        match op {
            AdaptiveOp::Preview(on) => {
                if on {
                    match self.project.interactive.first_state() {
                        Some(s) => self.start_score_preview(s)?,
                        None => self.notify(
                            NoticeLevel::Warning,
                            "The interactive score has no states yet: make one from a section or a scene",
                        ),
                    }
                } else {
                    self.stop_score_preview()?;
                }
            }
            AdaptiveOp::Go(state) => {
                if self.project.interactive.state(state).is_none() {
                    return Err(SessionError::Other("no such state".into()));
                }
                if self.adaptive.preview.is_none() || !self.transport.playing {
                    self.start_score_preview(state)?;
                } else {
                    self.plan_transition(state, None)?;
                }
            }
            AdaptiveOp::PlayStinger(id) => self.play_stinger(id)?,
            AdaptiveOp::SetValue { parameter, value } => {
                if let Some(p) = self.project.interactive.parameter(parameter) {
                    let v = p.clamp(value);
                    self.adaptive.values.insert(parameter, v);
                    if self.adaptive.preview.is_some() {
                        self.apply_layer_gains()?;
                    }
                    self.revision += 1;
                }
            }
            AdaptiveOp::Export(dir) => self.start_score_export(dir)?,
            AdaptiveOp::CancelExport => {
                if let Some(j) = &self.adaptive.export {
                    j.cancel();
                }
            }
            op => self.edit_score_op(op)?,
        }
        self.revision += 1;
        Ok(())
    }

    /// Replace the score in one undo step named `label` (nothing when it
    /// stays the same).
    fn edit_score(
        &mut self,
        label: &str,
        f: impl FnOnce(&mut InteractiveScore, &mut Self),
    ) -> Result<()> {
        let mut score = self.project.interactive.clone();
        f(&mut score, self);
        if score == self.project.interactive {
            return Ok(());
        }
        self.edit(Command::Batch {
            label: label.into(),
            commands: vec![Command::SetInteractiveScore {
                score: Box::new(score),
            }],
        })
    }

    fn edit_score_op(&mut self, op: AdaptiveOp) -> Result<()> {
        match op {
            AdaptiveOp::AddState(source) => self.edit_score("Add State", |score, s| {
                let id = s.project.ids.allocate();
                let name = state_name(&s.project, source, score);
                score.states.push(MusicState::new(id, name, source));
            }),
            AdaptiveOp::StatesFromSections => {
                self.edit_score("States from Sections", |score, s| {
                    let mut sections: Vec<_> = s.project.sections.clone();
                    sections.sort_by_key(|x| x.start);
                    for sec in sections {
                        let source = StateSource::Section { section: sec.id };
                        if score.states.iter().any(|st| st.source == source) {
                            continue;
                        }
                        let id = s.project.ids.allocate();
                        score
                            .states
                            .push(MusicState::new(id, sec.name.clone(), source));
                    }
                })
            }
            AdaptiveOp::StatesFromScenes => self.edit_score("States from Scenes", |score, s| {
                let scenes = s.project.launcher.scenes.clone();
                for scene in scenes {
                    let source = StateSource::Scene { scene: scene.id };
                    if score.states.iter().any(|st| st.source == source)
                        || s.project.scene_length(scene.id).is_none()
                    {
                        continue;
                    }
                    let id = s.project.ids.allocate();
                    let name = state_name(&s.project, source, score);
                    score.states.push(MusicState::new(id, name, source));
                }
            }),
            AdaptiveOp::SetState(state) => self.edit_score("Edit State", |score, _| {
                if let Some(s) = score.state_mut(state.id) {
                    *s = state;
                }
            }),
            AdaptiveOp::RemoveState(id) => self.edit_score("Remove State", |score, _| {
                score.states.retain(|s| s.id != id);
                // Rules naming it go with it.
                score
                    .rules
                    .retain(|r| r.from.state() != Some(id) && r.to.state() != Some(id));
            }),
            AdaptiveOp::MoveState { state, to } => self.edit_score("Move State", |score, _| {
                if let Some(i) = score.states.iter().position(|s| s.id == state) {
                    let s = score.states.remove(i);
                    let to = to.min(score.states.len());
                    score.states.insert(to, s);
                }
            }),
            AdaptiveOp::AddLayer { name, tracks } => {
                let tracks = if tracks.is_empty() {
                    let mut t: Vec<TrackId> = self.selection.tracks.iter().copied().collect();
                    t.sort_by_key(|id| self.project.tracks.iter().position(|x| x.id == *id));
                    t
                } else {
                    tracks
                };
                self.edit_score("Add Layer", |score, s| {
                    let id = s.project.ids.allocate();
                    let name = name.unwrap_or_else(|| format!("Layer {}", score.layers.len() + 1));
                    score.layers.push(MusicLayer { id, name, tracks });
                })
            }
            AdaptiveOp::SetLayer(layer) => self.edit_score("Edit Layer", |score, _| {
                if let Some(l) = score.layers.iter_mut().find(|l| l.id == layer.id) {
                    *l = layer;
                }
            }),
            AdaptiveOp::RemoveLayer(id) => self.edit_score("Remove Layer", |score, _| {
                score.layers.retain(|l| l.id != id);
                for p in &mut score.parameters {
                    p.curves.retain(|c| c.layer != id);
                }
            }),
            AdaptiveOp::AddRule { from, to } => {
                self.edit_score("Add Transition Rule", |score, s| {
                    let id = s.project.ids.allocate();
                    score.rules.push(TransitionRule::new(id, from, to));
                })
            }
            AdaptiveOp::SetRule(rule) => self.edit_score("Edit Transition Rule", |score, _| {
                if let Some(r) = score.rules.iter_mut().find(|r| r.id == rule.id) {
                    *r = rule;
                }
            }),
            AdaptiveOp::RemoveRule(id) => self.edit_score("Remove Transition Rule", |score, _| {
                score.rules.retain(|r| r.id != id);
            }),
            AdaptiveOp::MoveRule { rule, to } => {
                self.edit_score("Move Transition Rule", |score, _| {
                    if let Some(i) = score.rules.iter().position(|r| r.id == rule) {
                        let r = score.rules.remove(i);
                        let to = to.min(score.rules.len());
                        score.rules.insert(to, r);
                    }
                })
            }
            AdaptiveOp::AddParameter { name } => self.edit_score("Add Parameter", |score, s| {
                let id = s.project.ids.allocate();
                let name = name.unwrap_or_else(|| {
                    if score.parameters.is_empty() {
                        "Intensity".into()
                    } else {
                        format!("Parameter {}", score.parameters.len() + 1)
                    }
                });
                let mut p = ScoreParameter::new(id, name);
                // A first parameter brings the layers in one after another.
                if score.parameters.is_empty() && !score.layers.is_empty() {
                    let n = score.layers.len();
                    for (i, l) in score.layers.iter().enumerate().skip(1) {
                        let from = (i - 1) as f32 / n as f32;
                        p.curves
                            .push(LayerCurve::fade_in(l.id, from, i as f32 / n as f32));
                    }
                }
                score.parameters.push(p);
            }),
            AdaptiveOp::SetParameter(p) => self.edit_score("Edit Parameter", |score, _| {
                if let Some(x) = score.parameters.iter_mut().find(|x| x.id == p.id) {
                    *x = p;
                }
            }),
            AdaptiveOp::RemoveParameter(id) => self.edit_score("Remove Parameter", |score, _| {
                score.parameters.retain(|p| p.id != id);
            }),
            AdaptiveOp::AddStinger { slot } => {
                if !self.project.launcher.slots.contains_key(&slot) {
                    return Err(SessionError::Other(
                        "a stinger plays a launcher clip: that slot is empty".into(),
                    ));
                }
                self.edit_score("Add Stinger", |score, s| {
                    let id = s.project.ids.allocate();
                    let name = s
                        .project
                        .launcher
                        .slots
                        .get(&slot)
                        .and_then(|c| s.project.clips.get(c))
                        .map_or_else(|| "Stinger".into(), |c| c.name.clone());
                    score.stingers.push(Stinger {
                        id,
                        name,
                        slot,
                        sync: SyncPoint::Beat,
                        pickup: faderframe_timeline::MusicalTime::ZERO,
                    });
                })
            }
            AdaptiveOp::SetStinger(st) => self.edit_score("Edit Stinger", |score, _| {
                if let Some(x) = score.stingers.iter_mut().find(|x| x.id == st.id) {
                    *x = st;
                }
            }),
            AdaptiveOp::RemoveStinger(id) => self.edit_score("Remove Stinger", |score, _| {
                score.stingers.retain(|s| s.id != id);
            }),
            AdaptiveOp::SetStart(start) => self.edit_score("Starting State", |score, _| {
                score.start = start;
            }),
            AdaptiveOp::SetExport(export) => {
                self.edit_score("Score Export Settings", |score, _| {
                    score.export = export;
                })
            }
            _ => Ok(()),
        }
    }

    // --- the preview ---------------------------------------------------

    /// Engine samples at least as far ahead as a planned jump must be: what
    /// the render-ahead tiers may have rendered, two of their blocks, a
    /// device callback and a margin for the control thread.
    fn cue_margin(&self) -> i64 {
        let rate = f64::from(self.engine.sample_rate().max(1));
        let ahead = self.render_ahead.map_or(0.0, |d| d.as_secs_f64());
        let block = i64::from(self.engine.stream_buffer_size().max(256));
        (rate * (ahead + 0.06)) as i64 + 4 * block + 2048
    }

    /// Where a state plays, the scene's from `origin`.
    fn placement(&self, state: &MusicState, origin: i64) -> Option<Placement> {
        let rate = f64::from(self.engine.sample_rate().max(1));
        let span = self.project.state_span(state)?;
        Some(match state.source {
            StateSource::Section { .. } => {
                Placement::of_span(&span, state.looping, &self.project.timeline, rate)
            }
            StateSource::Scene { .. } => {
                let len = self.project.timeline.to_samples(span.end, rate);
                Placement::repeating(origin, len)
            }
        })
    }

    /// The loop held while a section state plays.
    fn held_loop_of(place: &Placement) -> (Option<LoopRange>, bool) {
        match place.kind {
            PlacementKind::Loops => (LoopRange::new(place.loop_start, place.loop_end), true),
            _ => (None, false),
        }
    }

    /// A state's named cues, as offsets from its start (engine samples).
    fn cue_offsets(&self, state: &MusicState) -> Vec<(String, i64)> {
        let rate = f64::from(self.engine.sample_rate().max(1));
        let Some(span) = self.project.state_span(state) else {
            return Vec::new();
        };
        let tl = &self.project.timeline;
        let start = tl.to_samples(span.start, rate);
        state
            .cues
            .iter()
            .map(|c| {
                (
                    c.name.clone(),
                    tl.to_samples(span.start + c.at, rate) - start,
                )
            })
            .collect()
    }

    fn marker_samples(&self) -> Vec<i64> {
        let rate = f64::from(self.engine.sample_rate().max(1));
        self.project
            .markers
            .iter()
            .map(|m| self.project.timeline.to_samples(m.position, rate))
            .collect()
    }

    /// Tracks the score's scene states play.
    fn score_scene_tracks(&self) -> Vec<TrackId> {
        let mut out: Vec<TrackId> = Vec::new();
        for s in &self.project.interactive.states {
            if let StateSource::Scene { scene } = s.source {
                for t in self.project.scene_tracks(scene) {
                    if !out.contains(&t) {
                        out.push(t);
                    }
                }
            }
        }
        out
    }

    /// Launch a scene state's clips (and stop the score's other scene
    /// tracks) on `quantize`.
    fn launch_state_scene(
        &mut self,
        scene: faderframe_core::SceneId,
        quantize: Quantize,
    ) -> Result<()> {
        let tracks = self.score_scene_tracks();
        for track in tracks {
            let key = SlotKey { track, scene };
            let cmd = match self.project.launcher.clip(track, scene) {
                Some(_) => LaunchCommand::Launch {
                    track,
                    slot: key.hash(),
                    quantize,
                    legato: false,
                    repeat: 0,
                },
                None => LaunchCommand::Stop { track, quantize },
            };
            self.engine.launch(cmd)?;
        }
        Ok(())
    }

    fn start_score_preview(&mut self, state: MusicStateId) -> Result<()> {
        let Some(s) = self.project.interactive.state(state).cloned() else {
            return Ok(());
        };
        let Some(span) = self.project.state_span(&s) else {
            self.notify(
                NoticeLevel::Warning,
                format!(
                    "‘{}’ plays nothing: its section or scene is gone or empty",
                    s.name
                ),
            );
            return Ok(());
        };
        if self.transport.playing {
            self.transport_action(TransportAction::Stop)?;
        }
        self.engine.set_cues(Cues::NONE)?;
        self.engine.transition_fade(None)?;
        self.engine.stop_at(None)?;
        let origin = match s.source {
            StateSource::Section { .. } => {
                self.engine.launch(LaunchCommand::BackToArrangement)?;
                self.transport_action(TransportAction::Locate(span.start))?;
                self.processing_position()
            }
            StateSource::Scene { scene } => {
                // Stopped, a launch takes effect at once where the
                // transport stands.
                self.launch_state_scene(scene, Quantize::None)?;
                self.processing_position()
            }
        };
        let Some(place) = self.placement(&s, origin) else {
            return Ok(());
        };
        let held = Self::held_loop_of(&place);
        self.engine.hold_loop(&self.project, Some(held))?;
        self.adaptive.preview = Some(Preview {
            current: Some(state),
            origin,
            segment: None,
            planned: None,
            held: Some(held),
            ended: false,
        });
        self.apply_layer_gains()?;
        self.play()?;
        Ok(())
    }

    fn stop_score_preview(&mut self) -> Result<()> {
        if self.adaptive.preview.take().is_none() {
            return Ok(());
        }
        if self.transport.playing {
            self.transport_action(TransportAction::Stop)?;
        }
        self.engine.set_cues(Cues::NONE)?;
        self.engine.transition_fade(None)?;
        self.engine.stop_at(None)?;
        self.engine.launch(LaunchCommand::BackToArrangement)?;
        self.engine.hold_loop(&self.project, None)?;
        self.engine
            .set_track_trims(&self.project, std::collections::HashMap::new())?;
        Ok(())
    }

    /// The layers' gains for the preview's parameter values.
    fn apply_layer_gains(&mut self) -> Result<()> {
        let gains = self
            .project
            .interactive
            .track_gains(&self.adaptive.values)
            .into_iter()
            .collect();
        self.engine.set_track_trims(&self.project, gains)?;
        Ok(())
    }

    /// Plan the move to `to` from the state playing (`sync`: the rule's
    /// unless given — a one-shot state's end).
    fn plan_transition(&mut self, to: MusicStateId, sync: Option<SyncPoint>) -> Result<()> {
        let Some(preview) = self.adaptive.preview.as_ref() else {
            return Ok(());
        };
        let (current, origin) = (preview.current, preview.origin);
        let score = self.project.interactive.clone();
        let Some(dest) = score.state(to).cloned() else {
            return Ok(());
        };
        let Some(cur) = current.and_then(|c| score.state(c)).cloned() else {
            return self.start_score_preview(to);
        };
        if current == Some(to) {
            // Asked for the state playing: what waits is called off.
            if preview.planned.is_some() {
                self.cancel_plan()?;
            }
            return Ok(());
        }
        let rate = f64::from(self.engine.sample_rate().max(1));
        let tl = self.project.timeline.clone();
        let rule = score.rule_between(current, to);
        let sync = sync.unwrap_or_else(|| rule.sync.clone());
        let Some(place) = self.placement(&cur, origin) else {
            return self.start_score_preview(to);
        };
        let pos = self.processing_position();
        let bpm = tl.tempo.bpm_at(tl.to_musical(pos, rate));
        let out_frames = (rule.fade_out.seconds(bpm) * rate) as i64;
        let in_frames = (rule.fade_in.seconds(bpm) * rate) as i64;
        let min_wait = self.cue_margin() + out_frames;
        let markers = self.marker_samples();
        let cues = self.cue_offsets(&cur);
        let cx = SyncContext {
            markers: &markers,
            cues: &cues,
        };
        // A sync point the state lacks (no marker, no such cue, past a
        // one-shot's end): its exit, else at once.
        let at = sync_point(&sync, &place, &tl, rate, &cx, pos, min_wait)
            .or_else(|| sync_point(&SyncPoint::Exit, &place, &tl, rate, &cx, pos, min_wait))
            .unwrap_or(pos + min_wait);
        // Where the destination begins.
        let entry_into = |p: &Placement| -> i64 {
            match rule.entry {
                faderframe_project::interactive::Entry::Start => p.start,
                faderframe_project::interactive::Entry::EntryCue => p.loop_start,
                faderframe_project::interactive::Entry::SameTime => {
                    let into = place.into_loop(at);
                    let len = (p.loop_end - p.loop_start).max(1);
                    p.loop_start + into.rem_euclid(len)
                }
            }
        };
        let segment = rule
            .segment
            .and_then(|g| score.state(g).cloned())
            .filter(|g| matches!(g.source, StateSource::Section { .. }))
            .and_then(|g| self.placement(&g, 0).map(|p| (g, p)));
        let mut list: Vec<Cue> = Vec::new();
        let leaving_scene = matches!(cur.source, StateSource::Scene { .. });
        let arrival = match dest.source {
            StateSource::Section { .. } => {
                let Some(dp) = self.placement(&dest, 0) else {
                    return Ok(());
                };
                let then_loop = Some(Self::held_loop_of(&dp));
                let to_pos = entry_into(&dp);
                match &segment {
                    Some((_, gp)) => {
                        list.push(Cue {
                            at,
                            to: gp.start,
                            then_loop: Some((None, false)),
                        });
                        list.push(Cue {
                            at: gp.loop_end,
                            to: to_pos,
                            then_loop,
                        });
                    }
                    None => list.push(Cue {
                        at,
                        to: to_pos,
                        then_loop,
                    }),
                }
                if leaving_scene {
                    self.engine.launch(LaunchCommand::Arrangement {
                        quantize: Quantize::At(at),
                    })?;
                }
                Arrival::Section
            }
            StateSource::Scene { scene } => {
                // The transport runs on; the loop goes off where the scene
                // comes in.
                if !leaving_scene {
                    list.push(Cue {
                        at,
                        to: at,
                        then_loop: Some((None, false)),
                    });
                }
                self.launch_state_scene(scene, Quantize::At(at))?;
                Arrival::Scene { start: at }
            }
        };
        let taken = self.engine.transport_snapshot().cues_taken;
        self.engine.set_cues(Cues::new(&list))?;
        let fade = (out_frames > 0 || in_frames > 0).then(|| TransitionFade {
            at,
            out_frames: out_frames.clamp(0, u32::MAX as i64) as u32,
            in_frames: in_frames.clamp(0, u32::MAX as i64) as u32,
            cue: list.first().is_some_and(|c| c.to != c.at),
        });
        self.engine.transition_fade(fade)?;
        if let Some(p) = self.adaptive.preview.as_mut() {
            p.planned = Some(Planned {
                to,
                via: segment.map(|(g, _)| g.id),
                at,
                // A cue that only changes the loop counts too (the
                // transport takes it on its frame).
                jumps: list.len() as u32,
                taken,
                arrival,
                when: Instant::now(),
            });
            p.ended = false;
        }
        Ok(())
    }

    /// Call off the transition waiting: no jump, no fade, the launcher's
    /// tracks on as they play (a scene's clips relaunched in place, the
    /// arrangement kept).
    fn cancel_plan(&mut self) -> Result<()> {
        let Some(p) = self.adaptive.preview.as_mut() else {
            return Ok(());
        };
        let Some(planned) = p.planned.take() else {
            return Ok(());
        };
        let current = p.current;
        self.engine.set_cues(Cues::NONE)?;
        self.engine.transition_fade(None)?;
        let scene = current
            .and_then(|c| self.project.interactive.state(c))
            .and_then(|s| match s.source {
                StateSource::Scene { scene } => Some(scene),
                StateSource::Section { .. } => None,
            });
        match scene {
            Some(scene) => {
                for track in self.score_scene_tracks() {
                    let key = SlotKey { track, scene };
                    let cmd = match self.project.launcher.clip(track, scene) {
                        Some(_) => LaunchCommand::Launch {
                            track,
                            slot: key.hash(),
                            quantize: Quantize::None,
                            legato: true,
                            repeat: 0,
                        },
                        None => LaunchCommand::Stop {
                            track,
                            quantize: Quantize::None,
                        },
                    };
                    self.engine.launch(cmd)?;
                }
            }
            // A scene was to come in: its tracks stay with the
            // arrangement.
            None if matches!(planned.arrival, Arrival::Scene { .. }) => {
                self.engine.launch(LaunchCommand::Arrangement {
                    quantize: Quantize::None,
                })?;
            }
            None => {}
        }
        Ok(())
    }

    fn play_stinger(&mut self, id: StingerId) -> Result<()> {
        let Some(st) = self.project.interactive.stinger(id).cloned() else {
            return Ok(());
        };
        if !self.project.launcher.slots.contains_key(&st.slot) {
            self.notify(
                NoticeLevel::Warning,
                format!("The stinger ‘{}’ plays an empty launcher slot", st.name),
            );
            return Ok(());
        }
        let rate = f64::from(self.engine.sample_rate().max(1));
        let tl = self.project.timeline.clone();
        let pos = self.processing_position();
        // Launcher tracks play live: a callback and a little more ahead.
        let margin =
            i64::from(self.engine.stream_buffer_size().max(256)) * 2 + (rate * 0.02) as i64;
        let pickup = tl.to_samples(st.pickup, rate);
        let current = self.adaptive.preview.as_ref().and_then(|p| {
            let s = p.current.and_then(|c| self.project.interactive.state(c))?;
            self.placement(s, p.origin).map(|pl| (s.clone(), pl))
        });
        let markers = self.marker_samples();
        let at = match &current {
            Some((s, place)) if self.transport.playing => {
                let cues = self.cue_offsets(s);
                let cx = SyncContext {
                    markers: &markers,
                    cues: &cues,
                };
                sync_point(&st.sync, place, &tl, rate, &cx, pos, margin + pickup)
                    .unwrap_or(pos + margin + pickup)
            }
            _ => pos + pickup,
        };
        let length = self
            .project
            .stinger_length(&st)
            .map_or(rate as i64, |l| tl.to_samples(l, rate));
        let track = st.slot.track;
        let slot = st.slot.hash();
        let start = at - pickup;
        self.engine.launch(LaunchCommand::Launch {
            track,
            slot,
            quantize: Quantize::At(start),
            legato: false,
            repeat: 0,
        })?;
        // Once: let go a clip's length after it starts.
        self.engine.launch(LaunchCommand::Release {
            track,
            slot,
            quantize: Quantize::At(start + length),
        })?;
        if !self.transport.playing {
            self.play()?;
        }
        Ok(())
    }

    /// Follow the preview (from the session tick): transitions arriving,
    /// jumps that went missing planned again, one-shot states' ends.
    pub(crate) fn tick_adaptive(&mut self) {
        if let Err(e) = self.run_adaptive() {
            self.notify(NoticeLevel::Error, format!("interactive score: {e}"));
        }
        if let Some(job) = &self.adaptive.export
            && job.is_finished()
            && let Some(job) = self.adaptive.export.take()
        {
            let result = job.join();
            match &result {
                Ok(r) => self.notify(
                    NoticeLevel::Info,
                    format!(
                        "The interactive score is exported: {} files in {}",
                        r.files.len(),
                        r.folder.display()
                    ),
                ),
                Err(e) => self.notify(NoticeLevel::Error, format!("score export: {e}")),
            }
            self.adaptive.last_export = Some(result);
            self.revision += 1;
        }
    }

    fn run_adaptive(&mut self) -> Result<()> {
        let Some(p) = self.adaptive.preview.as_ref() else {
            return Ok(());
        };
        // Its state gone (undone, removed): the preview ends.
        if p.current
            .is_some_and(|c| self.project.interactive.state(c).is_none())
        {
            return self.stop_score_preview();
        }
        // A new engine (the device reopened): the loop and gains again.
        if p.held != self.engine.held_loop() {
            let held = p.held;
            self.engine.hold_loop(&self.project, held)?;
        }
        let gains: std::collections::HashMap<TrackId, f32> = self
            .project
            .interactive
            .track_gains(&self.adaptive.values)
            .into_iter()
            .collect();
        self.engine.set_track_trims(&self.project, gains)?;
        let t = self.engine.transport_snapshot();
        if !t.playing {
            // Stopped (by hand, or a one-shot's end): nothing waits.
            if let Some(p) = self.adaptive.preview.as_mut() {
                if p.planned.take().is_some() {
                    self.revision += 1;
                }
                p.segment = None;
            }
            return Ok(());
        }
        let Some(p) = self.adaptive.preview.as_mut() else {
            return Ok(());
        };
        if let Some(q) = p.planned.clone() {
            let taken = t.cues_taken.wrapping_sub(q.taken);
            let arrived = if q.jumps > 0 {
                taken >= q.jumps
            } else {
                t.position >= q.at
            };
            if arrived {
                p.current = Some(q.to);
                p.segment = None;
                p.planned = None;
                p.ended = false;
                p.origin = match q.arrival {
                    Arrival::Scene { start } => start,
                    Arrival::Section => 0,
                };
                self.revision += 1;
            } else if taken >= 1 && q.via.is_some() {
                if p.segment != q.via {
                    p.segment = q.via;
                    self.revision += 1;
                }
            } else if q.jumps > 0
                && t.cues_waiting == 0
                && q.when.elapsed() > Duration::from_millis(100)
            {
                // The jumps went (a locate, a stop and play): plan again
                // from the state playing.
                p.planned = None;
                let to = q.to;
                return self.plan_transition(to, None);
            }
            return Ok(());
        }
        // A state that plays once: on to its `then` at its end, or stop
        // there.
        let Some(cur) = p
            .current
            .and_then(|c| self.project.interactive.state(c))
            .cloned()
        else {
            return Ok(());
        };
        if cur.looping || p.ended || matches!(cur.source, StateSource::Scene { .. }) {
            return Ok(());
        }
        p.ended = true;
        match cur.then {
            Some(next) => self.plan_transition(next, Some(SyncPoint::Exit)),
            None => {
                if let Some(place) = self.placement(&cur, 0) {
                    self.engine.stop_at(Some(place.loop_end))?;
                }
                Ok(())
            }
        }
    }

    // --- export --------------------------------------------------------

    fn start_score_export(&mut self, dir: PathBuf) -> Result<()> {
        if self.adaptive.export.is_some() {
            return Err(SessionError::Other(
                "an export of the score is under way".into(),
            ));
        }
        if self.project.interactive.states.is_empty() {
            return Err(SessionError::Other(
                "the interactive score has no states to export".into(),
            ));
        }
        if let Err(e) = self.settle_scenes() {
            tracing::warn!("mix scenes: {e}");
        }
        let project = self.render_copy();
        let job = export::start(project, dir).map_err(SessionError::Other)?;
        self.adaptive.export = Some(job);
        self.adaptive.last_export = None;
        Ok(())
    }

    /// Wait for the score's export to finish (tests, scripts).
    pub fn wait_for_score_export(&mut self, timeout: Duration) -> bool {
        let until = Instant::now() + timeout;
        while self.adaptive.export.is_some() {
            if Instant::now() > until {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
            self.tick_adaptive();
        }
        true
    }
}

/// A new state's name: its section's or scene's, numbered when taken.
fn state_name(
    project: &faderframe_project::Project,
    source: StateSource,
    score: &InteractiveScore,
) -> String {
    let base = match source {
        StateSource::Section { section } => project
            .sections
            .iter()
            .find(|s| s.id == section)
            .map_or_else(|| "State".into(), |s| s.name.clone()),
        StateSource::Scene { scene } => match project.launcher.scene_index(scene) {
            Some(i) if !project.launcher.scenes[i].name.is_empty() => {
                project.launcher.scenes[i].name.clone()
            }
            Some(i) => format!("Scene {}", i + 1),
            None => "State".into(),
        },
    };
    let taken = |n: &str| score.states.iter().any(|s| s.name.eq_ignore_ascii_case(n));
    if !taken(&base) {
        return base;
    }
    (2..)
        .map(|k| format!("{base} {k}"))
        .find(|n| !taken(n))
        .unwrap_or(base)
}
