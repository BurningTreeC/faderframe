//! Clip effects in the session: a clip's chain edited, rendered offline
//! and played (see [`faderframe_project::clip_fx`]).
//!
//! Edits ([`ClipFxOp`]) change a pending chain at once (what the editor
//! shows); a render starts when the chain has rested for a moment (a drag
//! renders once it pauses) in a worker, on a one-track copy of the
//! project holding the clip's original audio with the chain as inserts.
//! When it is done, the new file becomes a source and the clip plays it,
//! chain and all, in one "Clip Effects" step; a render that a newer edit
//! has overtaken is dropped. Removing every effect gives the clip its
//! audio back.
//!
//! A device can be shown in its own editor window
//! ([`Session::open_clip_fx_editor`]): the engine hosts an instance of
//! its slot outside the graph (as it does an album song's inserts), the
//! session answers for the slot wherever a plugin's owner is asked
//! ([`Session::clip_fx_slot`]), commands an editor window sends about it
//! (values, bypass, state, presets) become edits of the chain
//! ([`Session::clip_fx_commands`]), and what the plugin's own GUI changes
//! is taken into the chain once it rests — its state and every value it
//! moved — and rendered.

use crate::render::{self, RenderChannels, RenderJob, RenderRange, RenderSettings};
use crate::{NoticeLevel, Result, Session, SessionError};
use faderframe_audio_files::WavFormat;
use faderframe_core::{ChannelLayout, ClipId, ParameterId, PluginInstanceId, TrackId};
use faderframe_project::clip_fx::{ClipEffects, OriginalAudio};
use faderframe_project::{
    AudioClip, AudioSource, Clip, ClipContent, ClipFades, Command, PluginRef, PluginSlot, Project,
    SavedParameter, SourceSpec, Track, TrackColor, TrackKind,
};
use faderframe_timeline::MusicalTime;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// How long a chain rests before it renders.
const SETTLE: Duration = Duration::from_millis(300);
/// Seconds rendered after the clip (the effects' tails).
const TAIL_SECONDS: f32 = 2.0;

/// An edit of a clip's effects.
#[derive(Clone, Debug, PartialEq)]
pub enum ClipFxOp {
    /// A device at the end of the chain.
    Add(PluginRef),
    Remove(usize),
    Bypass(usize, bool),
    Move {
        from: usize,
        to: usize,
    },
    SetParameter {
        index: usize,
        parameter: ParameterId,
        value: f64,
    },
    /// Drop a parameter's explicit value: the device's own (its state's)
    /// counts again.
    ForgetParameter {
        index: usize,
        parameter: ParameterId,
    },
    /// A device's whole state and explicit values (a preset).
    SetState {
        index: usize,
        state: Option<String>,
        parameters: Vec<SavedParameter>,
    },
    /// Every effect off: the clip's audio back.
    Clear,
}

/// A device shown in its own editor window: its clip, and when its GUI
/// last changed something not yet taken into the chain.
struct Editor {
    clip: ClipId,
    touched: Option<Instant>,
    /// Its state was asked for (a sandboxed plugin answers later).
    asked: bool,
}

struct Pending {
    chain: Vec<PluginSlot>,
    changed: Instant,
    generation: u64,
}

struct Job {
    clip: ClipId,
    generation: u64,
    chain: Vec<PluginSlot>,
    original: OriginalAudio,
    job: RenderJob,
    path: PathBuf,
}

#[derive(Default)]
pub(crate) struct ClipFxState {
    pending: HashMap<ClipId, Pending>,
    jobs: Vec<Job>,
    generation: u64,
    /// Parameters by plugin (format and id).
    infos: HashMap<String, Vec<faderframe_plugin_host::ParameterInfo>>,
    /// The clip the editor shows.
    pub(crate) shown: Option<ClipId>,
    /// Devices in editor windows of their own, by slot.
    editors: HashMap<PluginInstanceId, Editor>,
    /// The session revision the editors' instances were last synced at.
    synced: u64,
}

fn key(p: &PluginRef) -> String {
    format!("{:?}:{}", p.format, p.id)
}

impl Session {
    /// The clip the clip-effects editor shows.
    pub fn clip_fx_clip(&self) -> Option<ClipId> {
        self.clip_fx.shown.filter(|c| {
            self.project
                .clip(*c)
                .is_some_and(|c| c.as_audio().is_some())
        })
    }

    /// A clip's chain as it is being edited (or as rendered).
    pub fn clip_fx_chain(&self, clip: ClipId) -> Vec<PluginSlot> {
        if let Some(p) = self.clip_fx.pending.get(&clip) {
            return p.chain.clone();
        }
        self.project
            .clip(clip)
            .and_then(|c| c.as_audio())
            .and_then(|a| a.effects.as_ref())
            .map(|e| e.chain.clone())
            .unwrap_or_default()
    }

    /// Is a clip's chain waiting to render or rendering?
    pub fn clip_fx_busy(&self, clip: ClipId) -> bool {
        self.clip_fx.pending.contains_key(&clip) || self.clip_fx.jobs.iter().any(|j| j.clip == clip)
    }

    /// A device's parameters (made once per plugin and kept).
    pub fn clip_fx_parameters(
        &mut self,
        plugin: &PluginRef,
    ) -> Vec<faderframe_plugin_host::ParameterInfo> {
        let k = key(plugin);
        if let Some(v) = self.clip_fx.infos.get(&k) {
            return v.clone();
        }
        let v = self.engine.describe_parameters(plugin).unwrap_or_default();
        self.clip_fx.infos.insert(k, v.clone());
        v
    }

    /// A device's parameters if they are known already (views).
    pub fn clip_fx_known_parameters(
        &self,
        plugin: &PluginRef,
    ) -> Option<&[faderframe_plugin_host::ParameterInfo]> {
        self.clip_fx.infos.get(&key(plugin)).map(Vec::as_slice)
    }

    /// Show a clip in the clip-effects editor.
    pub(crate) fn open_clip_fx(&mut self, clip: ClipId) -> Result<()> {
        if self.project.clip(clip).and_then(|c| c.as_audio()).is_none() {
            return Err(SessionError::Other("clip effects go on audio clips".into()));
        }
        self.clip_fx.shown = Some(clip);
        for slot in self.clip_fx_chain(clip) {
            self.clip_fx_parameters(&slot.plugin);
        }
        self.workspace_action(crate::WorkspaceAction::ShowView(
            faderframe_workspace::ViewId::clip_fx(),
        ))?;
        self.revision += 1;
        Ok(())
    }

    /// Edit a clip's effects (see the module docs).
    pub fn edit_clip_fx(&mut self, clip: ClipId, op: ClipFxOp) -> Result<()> {
        let Some(c) = self.project.clip(clip).cloned() else {
            return Err(SessionError::Other(format!("no clip {clip}")));
        };
        let Some(a) = c.as_audio().cloned() else {
            return Err(SessionError::Other("clip effects go on audio clips".into()));
        };
        let mut chain = self.clip_fx_chain(clip);
        match op {
            ClipFxOp::Add(plugin) => {
                self.clip_fx_parameters(&plugin);
                chain.push(PluginSlot {
                    id: self.project.ids.allocate(),
                    plugin,
                    bypass: false,
                    parameters: Vec::new(),
                    state: None,
                    sidechain: None,
                });
            }
            ClipFxOp::Remove(i) if i < chain.len() => {
                chain.remove(i);
            }
            ClipFxOp::Bypass(i, on) if i < chain.len() => chain[i].bypass = on,
            ClipFxOp::Move { from, to } if from < chain.len() => {
                let s = chain.remove(from);
                chain.insert(to.min(chain.len()), s);
            }
            ClipFxOp::SetParameter {
                index,
                parameter,
                value,
            } if index < chain.len() => {
                // Explicit values are applied after the state: it stays
                // (what the device's own editor set is kept).
                let slot = &mut chain[index];
                match slot.parameters.iter_mut().find(|p| p.id == parameter) {
                    Some(p) => p.value = value,
                    None => slot.parameters.push(SavedParameter {
                        id: parameter,
                        value,
                    }),
                }
            }
            ClipFxOp::ForgetParameter { index, parameter } if index < chain.len() => {
                chain[index].parameters.retain(|p| p.id != parameter);
            }
            ClipFxOp::SetState {
                index,
                state,
                parameters,
            } if index < chain.len() => {
                chain[index].state = state;
                chain[index].parameters = parameters;
            }
            ClipFxOp::Clear => chain.clear(),
            _ => return Ok(()),
        }
        if chain.is_empty() {
            // Nothing left: the original back (an undo step), nothing
            // pending.
            self.clip_fx.pending.remove(&clip);
            if let Some(fx) = a.effects.as_deref() {
                let mut restored = a.clone();
                fx.original.restore(&mut restored, fx.trim(&a));
                self.set_audio("Remove Clip Effects", &c, c.start, restored)?;
            }
            self.revision += 1;
            return Ok(());
        }
        self.clip_fx.generation += 1;
        let generation = self.clip_fx.generation;
        self.clip_fx.pending.insert(
            clip,
            Pending {
                chain,
                changed: Instant::now(),
                generation,
            },
        );
        self.revision += 1;
        Ok(())
    }

    /// Show device `index` of `clip`'s chain in its own editor window
    /// (its GUI, or the generic one when `generic` or it has none).
    pub(crate) fn open_clip_fx_editor(
        &mut self,
        clip: ClipId,
        index: usize,
        generic: bool,
    ) -> Result<()> {
        let Some(slot) = self.clip_fx_chain(clip).get(index).cloned() else {
            return Err(SessionError::Other("no such clip effect".into()));
        };
        let track = self
            .project
            .clip(clip)
            .map(|c| c.track)
            .ok_or_else(|| SessionError::Other(format!("no clip {clip}")))?;
        if !self.engine.host_plugin(&slot) {
            return Err(SessionError::Other(format!(
                "{} could not be loaded",
                slot.plugin.name
            )));
        }
        // Hosted before (its window closed since): what the chain says now.
        self.engine.sync_plugin_slot(&slot);
        self.clip_fx.editors.entry(slot.id).or_insert(Editor {
            clip,
            touched: None,
            asked: false,
        });
        self.engine
            .keep_plugins(self.clip_fx.editors.keys().copied().collect());
        self.ui_requests.push(crate::UiRequest::PluginEditor {
            track,
            plugin: slot.id,
            generic,
        });
        self.revision += 1;
        Ok(())
    }

    /// A device in an editor window of its own: its clip, its place in the
    /// chain and its slot (as being edited).
    pub fn clip_fx_slot(&self, plugin: PluginInstanceId) -> Option<(ClipId, usize, &PluginSlot)> {
        let clip = self.clip_fx.editors.get(&plugin)?.clip;
        let chain: &[PluginSlot] = match self.clip_fx.pending.get(&clip) {
            Some(p) => &p.chain,
            None => &self.project.clip(clip)?.as_audio()?.effects.as_ref()?.chain,
        };
        chain
            .iter()
            .enumerate()
            .find(|(_, s)| s.id == plugin)
            .map(|(i, s)| (clip, i, s))
    }

    /// A device's name as the views show it, for one in a clip's chain.
    pub(crate) fn clip_fx_device_name(&self, plugin: PluginInstanceId) -> Option<String> {
        let (clip, _, slot) = self.clip_fx_slot(plugin)?;
        let chain: &[PluginSlot] = match self.clip_fx.pending.get(&clip) {
            Some(p) => &p.chain,
            None => &self.project.clip(clip)?.as_audio()?.effects.as_ref()?.chain,
        };
        Some(faderframe_project::device_name(chain, slot))
    }

    /// The chain edits a command about devices in editor windows of their
    /// own stands for (editor windows send track commands; device editors
    /// batch several values). `None` when it names anything else.
    pub(crate) fn clip_fx_commands(&self, cmd: &Command) -> Option<Vec<(ClipId, ClipFxOp)>> {
        if self.clip_fx.editors.is_empty() {
            return None;
        }
        match cmd {
            Command::Batch { commands, .. } if !commands.is_empty() => commands
                .iter()
                .map(|c| self.clip_fx_commands(c))
                .collect::<Option<Vec<_>>>()
                .map(|v| v.into_iter().flatten().collect()),
            _ => self.clip_fx_command(cmd).map(|e| vec![e]),
        }
    }

    /// The chain edit one command about a device in an editor window of
    /// its own stands for.
    fn clip_fx_command(&self, cmd: &Command) -> Option<(ClipId, ClipFxOp)> {
        let (plugin, op): (_, Box<dyn Fn(usize) -> ClipFxOp>) = match cmd {
            Command::SetPluginParameter {
                plugin,
                parameter,
                value,
                ..
            } => {
                let (parameter, value) = (*parameter, *value);
                (
                    *plugin,
                    Box::new(move |index| match value {
                        Some(value) => ClipFxOp::SetParameter {
                            index,
                            parameter,
                            value,
                        },
                        None => ClipFxOp::ForgetParameter { index, parameter },
                    }),
                )
            }
            Command::SetPluginBypass { plugin, bypass, .. } => {
                let bypass = *bypass;
                (
                    *plugin,
                    Box::new(move |index| ClipFxOp::Bypass(index, bypass)),
                )
            }
            Command::SetPluginState {
                plugin,
                state,
                parameters,
                ..
            } => {
                let (state, parameters) = (state.clone(), parameters.clone());
                (
                    *plugin,
                    Box::new(move |index| ClipFxOp::SetState {
                        index,
                        state: state.clone(),
                        parameters: parameters.clone(),
                    }),
                )
            }
            _ => return None,
        };
        let (clip, index, _) = self.clip_fx_slot(plugin)?;
        Some((clip, op(index)))
    }

    /// Moves made in a device's own GUI: those of devices in editor
    /// windows of their own are noted (taken in once they rest), the
    /// others are handed back.
    pub(crate) fn clip_fx_editor_edits(
        &mut self,
        edits: Vec<(PluginInstanceId, faderframe_plugin_host::EditorEdit)>,
    ) -> Vec<(PluginInstanceId, faderframe_plugin_host::EditorEdit)> {
        if self.clip_fx.editors.is_empty() {
            return edits;
        }
        edits
            .into_iter()
            .filter(|(plugin, _)| !self.note_clip_fx_touched(*plugin))
            .collect()
    }

    /// A device in an editor window of its own changed (its GUI moved a
    /// value, its state is dirty): true when it is one.
    pub(crate) fn note_clip_fx_touched(&mut self, plugin: PluginInstanceId) -> bool {
        match self.clip_fx.editors.get_mut(&plugin) {
            Some(e) => {
                e.touched = Some(Instant::now());
                true
            }
            None => false,
        }
    }

    /// Per tick: instances of devices gone from their chains go; instances
    /// follow their slots (undo, the view's bars); what a device's GUI
    /// changed is taken into its chain once it rests.
    fn tick_clip_fx_editors(&mut self) {
        if self.clip_fx.editors.is_empty() {
            return;
        }
        let gone: Vec<PluginInstanceId> = self
            .clip_fx
            .editors
            .keys()
            .filter(|id| self.clip_fx_slot(**id).is_none())
            .copied()
            .collect();
        for id in &gone {
            self.clip_fx.editors.remove(id);
            self.engine.release_plugin(*id);
        }
        if !gone.is_empty() {
            self.engine
                .keep_plugins(self.clip_fx.editors.keys().copied().collect());
        }
        if self.clip_fx.synced != self.revision {
            self.clip_fx.synced = self.revision;
            let slots: Vec<PluginSlot> = self
                .clip_fx
                .editors
                .keys()
                .filter_map(|id| self.clip_fx_slot(*id).map(|(_, _, s)| s.clone()))
                .collect();
            for slot in &slots {
                self.engine.sync_plugin_slot(slot);
            }
        }
        let rested: Vec<PluginInstanceId> = self
            .clip_fx
            .editors
            .iter()
            .filter(|(_, e)| !e.asked && e.touched.is_some_and(|t| t.elapsed() >= SETTLE))
            .map(|(id, _)| *id)
            .collect();
        for id in rested {
            if let Some(e) = self.clip_fx.editors.get_mut(&id) {
                e.touched = None;
            }
            if self.engine.request_plugin_state(id) {
                if let Some(e) = self.clip_fx.editors.get_mut(&id) {
                    e.asked = true;
                }
            } else {
                let state = self.engine.plugin_state(id);
                self.take_clip_fx_state(id, state);
            }
        }
        let asked: Vec<PluginInstanceId> = self
            .clip_fx
            .editors
            .iter()
            .filter(|(_, e)| e.asked)
            .map(|(id, _)| *id)
            .collect();
        for id in asked {
            let answer = self.engine.take_plugin_state(id);
            if answer.is_none() && !self.engine.plugin_failed(id) {
                continue;
            }
            if let Some(e) = self.clip_fx.editors.get_mut(&id) {
                e.asked = false;
            }
            if let Some(state) = answer {
                self.take_clip_fx_state(id, state);
            }
        }
    }

    /// Put what a device's own GUI changed into its chain: its state, and
    /// every value it moved (explicit values are applied after the state,
    /// so they must follow; the view shows them), then render.
    fn take_clip_fx_state(&mut self, plugin: PluginInstanceId, state: Option<String>) {
        let Some((clip, index, _)) = self.clip_fx_slot(plugin) else {
            return;
        };
        let mut chain = self.clip_fx_chain(clip);
        let Some(slot) = chain.get_mut(index) else {
            return;
        };
        let before = (slot.state.clone(), slot.parameters.clone());
        if let Some(state) = &state {
            slot.state = Some(state.clone());
            self.engine.note_plugin_state(plugin, state);
        }
        let infos = self.clip_fx_parameters(&slot.plugin);
        for info in infos {
            let Some(v) = self.engine.plugin_parameter_value(plugin, info.id) else {
                continue;
            };
            match slot.parameters.iter_mut().find(|p| p.id == info.id) {
                Some(p) => p.value = v,
                None if (v - info.default).abs() > 1e-9 => slot.parameters.push(SavedParameter {
                    id: info.id,
                    value: v,
                }),
                None => continue,
            }
            self.engine.note_plugin_parameter(plugin, info.id, v);
        }
        if (slot.state.clone(), slot.parameters.clone()) == before {
            return;
        }
        self.clip_fx.generation += 1;
        let generation = self.clip_fx.generation;
        // It rested already: render at once.
        let changed = Instant::now()
            .checked_sub(SETTLE)
            .unwrap_or_else(Instant::now);
        self.clip_fx.pending.insert(
            clip,
            Pending {
                chain,
                changed,
                generation,
            },
        );
        self.revision += 1;
    }

    /// Render a clip's chain again (its original audio changed).
    pub(crate) fn rerender_clip_fx(&mut self, clip: ClipId) {
        let chain = self.clip_fx_chain(clip);
        if chain.is_empty() {
            return;
        }
        self.clip_fx.generation += 1;
        let generation = self.clip_fx.generation;
        self.clip_fx.pending.insert(
            clip,
            Pending {
                chain,
                changed: Instant::now(),
                generation,
            },
        );
    }

    /// Start renders of chains that have rested; place the finished ones
    /// (from the session tick).
    pub(crate) fn poll_clip_fx(&mut self) {
        self.tick_clip_fx_editors();
        let ready: Vec<ClipId> = self
            .clip_fx
            .pending
            .iter()
            .filter(|(c, p)| {
                p.changed.elapsed() >= SETTLE
                    && !self
                        .clip_fx
                        .jobs
                        .iter()
                        .any(|j| j.clip == **c && j.generation == p.generation)
            })
            .map(|(c, _)| *c)
            .collect();
        for clip in ready {
            if let Err(e) = self.start_clip_fx_render(clip) {
                self.clip_fx.pending.remove(&clip);
                self.notify(NoticeLevel::Error, format!("clip effects: {e}"));
            }
        }
        let mut i = 0;
        while i < self.clip_fx.jobs.len() {
            if !self.clip_fx.jobs[i].job.is_finished() {
                i += 1;
                continue;
            }
            let j = self.clip_fx.jobs.remove(i);
            let latest = self
                .clip_fx
                .pending
                .get(&j.clip)
                .is_none_or(|p| p.generation == j.generation);
            let result = j.job.join();
            if !latest {
                // Overtaken by a newer edit.
                let _ = std::fs::remove_file(&j.path);
                continue;
            }
            self.clip_fx.pending.remove(&j.clip);
            let placed = result
                .map_err(|e| SessionError::Other(e.to_string()))
                .and_then(|_| self.place_clip_fx(j.clip, j.chain, j.original, &j.path));
            if let Err(e) = placed {
                self.notify(NoticeLevel::Error, format!("clip effects: {e}"));
            }
            self.revision += 1;
        }
    }

    fn start_clip_fx_render(&mut self, clip: ClipId) -> Result<()> {
        let Some(p) = self.clip_fx.pending.get(&clip) else {
            return Ok(());
        };
        let (chain, generation) = (p.chain.clone(), p.generation);
        let c = self
            .project
            .clip(clip)
            .cloned()
            .ok_or_else(|| SessionError::Other("the clip is gone".into()))?;
        let a = c
            .as_audio()
            .ok_or_else(|| SessionError::Other("not an audio clip".into()))?;
        let original = a
            .effects
            .as_ref()
            .map_or_else(|| OriginalAudio::of(a), |e| e.original.clone());
        let whole = self.absolute_copy();
        let source = whole
            .sources
            .get(&original.source)
            .cloned()
            .ok_or_else(|| SessionError::Other("the clip's audio is missing".into()))?;
        let channels = match &source.spec {
            SourceSpec::File { channels, .. } => usize::from(*channels),
            SourceSpec::Generated { generator } => generator.channels(),
        };
        // A project of its own: the original audio, the chain as inserts.
        let rate = self.project.sample_rate;
        let mut p = Project::new("Clip Effects", rate);
        p.timeline = whole.timeline.clone();
        // Ids of the copy must not meet the chain's.
        p.ids = whole.ids.clone();
        p.sources.insert(source.id, source);
        let track: TrackId = p.ids.allocate();
        let mut t = Track::new(track, TrackKind::Audio, "Clip", TrackColor::palette(0))
            .with_layout(if channels == 1 {
                ChannelLayout::Mono
            } else {
                ChannelLayout::Stereo
            });
        t.inserts = chain.clone();
        let clip_id: ClipId = p.ids.allocate();
        t.clips.push(clip_id);
        p.tracks.insert(0, t);
        p.clips.insert(
            clip_id,
            Clip {
                id: clip_id,
                track,
                name: c.name.clone(),
                color: None,
                start: MusicalTime::ZERO,
                muted: false,
                content: ClipContent::Audio(AudioClip {
                    source: original.source,
                    source_offset: original.source_offset,
                    length: original.length,
                    gain_db: 0.0,
                    fades: ClipFades::default(),
                    stretch: original.stretch,
                    reversed: original.reversed,
                    warp: original.warp.clone(),
                    pitch: original.pitch.clone(),
                    effects: None,
                    spectral: None,
                }),
            },
        );
        let (copy, start, end) = render::track_render_project(&p, track)
            .ok_or_else(|| SessionError::Other("nothing to render".into()))?;
        std::fs::create_dir_all(&self.media_dir)
            .map_err(|e| SessionError::Other(format!("{}: {e}", self.media_dir.display())))?;
        let path = crate::media::unique_path(&self.media_dir, &format!("{} FX", c.name));
        let settings = RenderSettings {
            range: RenderRange::Span { start, end },
            channels: if channels == 1 {
                RenderChannels::First
            } else {
                RenderChannels::Stereo
            },
            sample_rate: rate,
            tail_seconds: TAIL_SECONDS,
            normalize_db: None,
            format: WavFormat::Float32,
            dither: faderframe_audio_files::Dither::Off,
            report: false,
            // Its own file (a name the session chose): no one to ask.
            overwrite: crate::render::Overwrite::Always,
            ..RenderSettings::defaults_for(&copy, path.clone())
        };
        let job = render::start(copy, settings).map_err(|e| SessionError::Other(e.to_string()))?;
        self.clip_fx.jobs.push(Job {
            clip,
            generation,
            chain,
            original,
            job,
            path,
        });
        Ok(())
    }

    fn place_clip_fx(
        &mut self,
        clip: ClipId,
        chain: Vec<PluginSlot>,
        original: OriginalAudio,
        path: &std::path::Path,
    ) -> Result<()> {
        let wav = faderframe_audio_files::wavstream::WavFile::open(path)
            .map_err(|e| SessionError::Other(format!("{}: {e}", path.display())))?;
        let (frames, channels, rate) = (wav.frames() as i64, wav.channels(), wav.sample_rate());
        let Some(c) = self.project.clip(clip).cloned() else {
            return Ok(()); // deleted meanwhile
        };
        let Some(mut a) = c.as_audio().cloned() else {
            return Ok(());
        };
        let trim = a.effects.as_ref().map_or(0, |e| e.trim(&a));
        let source = AudioSource {
            id: self.project.ids.allocate(),
            name: path
                .file_stem()
                .map_or_else(|| c.name.clone(), |s| s.to_string_lossy().to_string()),
            spec: SourceSpec::File {
                path: path.to_path_buf(),
                channels: channels as u16,
                frames,
                sample_rate: rate,
            },
        };
        // Renders start where the range does (their latency is taken off).
        let offset = 0;
        a.source = source.id;
        a.source_offset = (offset + trim.max(0)).min(frames - 1).max(0);
        a.length = a.length.min(frames - a.source_offset).max(1);
        a.warp = None;
        a.pitch = None;
        a.reversed = false;
        a.effects = Some(Box::new(ClipEffects {
            chain,
            original,
            rendered_offset: offset,
        }));
        let cmds = vec![
            Command::AddSource {
                source: Box::new(source),
            },
            Command::SetClipContent {
                clip,
                start: c.start,
                content: Box::new(ClipContent::Audio(a)),
            },
        ];
        self.batch("Clip Effects", cmds)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use faderframe_engine::EngineConfig;
    use faderframe_project::PluginFormat;

    /// What a device's own GUI changes reaches its chain once it rests —
    /// the moved value made explicit, so the render and the view have it —
    /// and the instance is not pushed back to the old value meanwhile.
    #[test]
    fn what_a_devices_own_gui_changes_reaches_the_chain() {
        let mut s = Session::demo(EngineConfig::default()).unwrap();
        let clip = s
            .project()
            .tracks
            .iter()
            .find(|t| t.name == "Pad")
            .unwrap()
            .clips[0];
        let utility = PluginRef {
            format: PluginFormat::Builtin,
            id: faderframe_core::builtin::GAIN.into(),
            name: "Utility".into(),
        };
        s.edit_clip_fx(clip, ClipFxOp::Add(utility)).unwrap();
        s.open_clip_fx_editor(clip, 0, false).unwrap();
        s.poll_clip_fx();
        let mut slot = s.clip_fx_chain(clip)[0].clone();
        assert!(slot.parameters.is_empty());
        // The GUI turns the gain down (as an editor does: on the instance).
        slot.parameters.push(SavedParameter {
            id: ParameterId(0),
            value: -3.0,
        });
        s.engine.sync_plugin_slot(&slot);
        assert!(s.note_clip_fx_touched(slot.id));
        s.poll_clip_fx();
        assert!(
            s.clip_fx_chain(clip)[0].parameters.is_empty(),
            "not before it rests"
        );
        std::thread::sleep(SETTLE + Duration::from_millis(20));
        s.poll_clip_fx();
        let taken = &s.clip_fx_chain(clip)[0];
        assert_eq!(
            taken
                .parameters
                .iter()
                .find(|p| p.id == ParameterId(0))
                .map(|p| p.value),
            Some(-3.0)
        );
        assert!(taken.state.is_some(), "its state too");
        // Synced again: the instance keeps the value.
        s.revision += 1;
        s.poll_clip_fx();
        assert_eq!(
            s.engine.plugin_parameter_value(slot.id, ParameterId(0)),
            Some(-3.0)
        );
    }
}
