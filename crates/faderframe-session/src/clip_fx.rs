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

use crate::render::{self, RenderChannels, RenderJob, RenderRange, RenderSettings};
use crate::{NoticeLevel, Result, Session, SessionError};
use faderframe_audio_files::WavFormat;
use faderframe_core::{ChannelLayout, ClipId, ParameterId, TrackId};
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
    /// Every effect off: the clip's audio back.
    Clear,
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
                let slot = &mut chain[index];
                // A device's state would override the value: its
                // parameters are what it is set by here.
                slot.state = None;
                match slot.parameters.iter_mut().find(|p| p.id == parameter) {
                    Some(p) => p.value = value,
                    None => slot.parameters.push(SavedParameter {
                        id: parameter,
                        value,
                    }),
                }
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

    /// Start renders of chains that have rested; place the finished ones
    /// (from the session tick).
    pub(crate) fn poll_clip_fx(&mut self) {
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
