//! ARA hosting: the project's ARA inserts, one ARA document per plug-in
//! factory, kept in step with the project.
//!
//! [`AraHost::prepare`] turns the project into what each document should
//! hold: a musical context (the tempo map, bar signatures, keys and the
//! chord track), a region sequence per track with an ARA insert, an audio
//! source per media file its clips play (read by the plug-in's audio
//! readers straight from the file, never through the live streams), an
//! audio modification per track and file (so splits, trims and moves keep
//! the plug-in's edits; an undone delete gets them back), and a playback
//! region per clip piece (a plain clip is one piece, a take folder's comp
//! one per piece). A document is made when the first instance of its
//! plug-in is bound (`PluginHost::ensure`), restoring the project's
//! archive in its first edit cycle; every later sync runs one edit cycle
//! when something changed. Renderers whose playback regions change (a clip
//! added or removed on their track) are deactivated first (ARA's rule) and
//! the graph is rebuilt, which activates them again.

use crate::plugins::{decode_state, encode_state};
use crate::snapshot::{Source as EngineSource, SourceMap};
use faderframe_audio_files::AudioData;
use faderframe_audio_files::wavstream::WavFile;
use faderframe_core::{AudioSourceId, ClipId, PluginInstanceId, TrackId};
use faderframe_plugin_host::PluginInstance;
use faderframe_plugin_host::ara::content::{OrderedQuarters, circle_of_fifths};
use faderframe_plugin_host::ara::{
    Analysis, Archive, Assignment, BarSignature, Chord, Context, Document, KeySignature, Model,
    Modification, Note, PlaybackRequest, Region, Renderer, Roles, Samples, Sequence, Source,
    TempoEntry, ffi,
};
use faderframe_project::{
    ClipContent, PluginFormat as ProjectFormat, Project, SourceSpec, TrackColor, ara::AraArchive,
};
use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

/// An ARA insert as the project has it.
#[derive(Clone, Debug, PartialEq)]
struct SlotInfo {
    track: TrackId,
    plugin: (ProjectFormat, String),
    bypass: bool,
}

/// One track's share of a document's model.
#[derive(Clone, Default)]
struct TrackPart {
    sequence: Option<Sequence>,
    sources: Vec<Source>,
    modifications: Vec<Modification>,
    regions: Vec<Region>,
}

struct Doc {
    document: Document,
    /// The plugins whose instances bind to it.
    plugins: HashSet<(ProjectFormat, String)>,
    /// Hashes of the archives it restored or stored (0: made without one):
    /// a project archive it does not know means another project.
    known: HashSet<u64>,
}

struct Bound {
    factory: String,
    track: TrackId,
    renderer: Renderer,
}

/// The state of a document dropped because nothing used it any more (its
/// insert removed, an undone "Open with ARA"): a document made again for
/// the same project starts from it, so undo and redo keep the edits.
struct Parked {
    archive: Archive,
    plugin: String,
    vendor: String,
    version: String,
    /// The project archives its document knew.
    known: HashSet<u64>,
    unsaved: bool,
}

/// A media file an audio source reads (opened once, shared by readers).
struct OpenFile {
    path: PathBuf,
    samples: Arc<dyn Samples>,
    frames: i64,
    channels: u32,
    rate: f64,
}

/// The engine's ARA side (owned by [`crate::plugins::PluginHost`]).
#[derive(Default)]
pub(crate) struct AraHost {
    slots: HashMap<PluginInstanceId, SlotInfo>,
    parts: HashMap<TrackId, TrackPart>,
    context: Context,
    name: Option<String>,
    archives: Vec<AraArchive>,
    docs: HashMap<String, Doc>,
    bound: HashMap<PluginInstanceId, Bound>,
    /// Instances that are not ARA plugins after all (bound as plain
    /// inserts; not tried again while their slot stays).
    failed: HashSet<PluginInstanceId>,
    files: HashMap<AudioSourceId, OpenFile>,
    requests: Vec<PlaybackRequest>,
    analysis: Vec<(String, Analysis)>,
    /// Documents that changed state since the archives were captured.
    unsaved: HashSet<String>,
    parked: HashMap<String, Parked>,
}

/// FNV-1a: an archive's identity (no security meaning).
fn hash(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

fn color(c: TrackColor) -> [f32; 3] {
    [
        f32::from(c.r) / 255.0,
        f32::from(c.g) / 255.0,
        f32::from(c.b) / 255.0,
    ]
}

pub(crate) fn sequence_key(track: TrackId) -> String {
    format!("t:{}", track.raw())
}

fn source_key(source: AudioSourceId) -> String {
    format!("src:{}", source.raw())
}

fn modification_key(track: TrackId, source: AudioSourceId) -> String {
    format!("mod:{}:{}", track.raw(), source.raw())
}

/// Regions of a clip are named `r:<clip>.<piece>@<modification>`.
fn region_key(clip: ClipId, piece: usize, modification: &str) -> String {
    format!("r:{}.{piece}@{modification}", clip.raw())
}

fn region_prefix(clip: ClipId) -> String {
    format!("r:{}.", clip.raw())
}

/// A WAV file read through its own handle (positional reads: readers on
/// several threads at once).
struct FileSamples {
    file: WavFile,
}

thread_local! {
    static SCRATCH: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

impl Samples for FileSamples {
    fn read(&self, start: i64, out: &mut [&mut [f32]]) -> bool {
        let frames = out.iter().map(|c| c.len()).min().unwrap_or(0);
        let skip = if start < 0 {
            (start.unsigned_abs() as usize).min(frames)
        } else {
            0
        };
        for c in out.iter_mut() {
            c[..skip].fill(0.0);
        }
        if skip == frames {
            return true;
        }
        let mut rest: Vec<&mut [f32]> = out.iter_mut().map(|c| &mut c[skip..frames]).collect();
        let from = (start + skip as i64) as u64;
        SCRATCH.with(|s| {
            let mut scratch = s.borrow_mut();
            match self.file.read(from, &mut rest, &mut scratch) {
                Ok(()) => true,
                Err(e) => {
                    tracing::warn!("ARA: reading {}: {e}", self.file.path().display());
                    rest.iter_mut().for_each(|c| c.fill(0.0));
                    false
                }
            }
        })
    }
}

/// Generated material (decoded at the engine rate).
struct MemorySamples(Arc<AudioData>);

impl Samples for MemorySamples {
    fn read(&self, start: i64, out: &mut [&mut [f32]]) -> bool {
        let n = self.0.num_channels().max(1);
        for (c, o) in out.iter_mut().enumerate() {
            let src = self.0.channel(c.min(n - 1));
            for (i, v) in o.iter_mut().enumerate() {
                let k = start + i as i64;
                *v = if k >= 0 && (k as usize) < src.len() {
                    src[k as usize]
                } else {
                    0.0
                };
            }
        }
        true
    }
}

impl AraHost {
    /// The project's ARA inserts and what their documents should hold.
    pub fn prepare(&mut self, project: &Project, sources: &SourceMap) {
        self.name = Some(project.name.clone());
        self.archives = project.ara.archives.clone();
        self.context = context_of(project);
        self.slots.clear();
        for t in &project.tracks {
            if t.freeze.is_some() {
                // A frozen track plays its render; its devices are unloaded.
                continue;
            }
            for s in t.slots() {
                if project.ara.slots.contains(&s.id) {
                    self.slots.insert(
                        s.id,
                        SlotInfo {
                            track: t.id,
                            plugin: (s.plugin.format, s.plugin.id.clone()),
                            bypass: s.bypass,
                        },
                    );
                }
            }
        }
        let tracks: HashSet<TrackId> = self.slots.values().map(|s| s.track).collect();
        self.files.retain(|id, _| project.sources.contains_key(id));
        let mut parts = HashMap::new();
        for (order, t) in project.tracks.iter().enumerate() {
            if tracks.contains(&t.id) {
                parts.insert(t.id, self.part_of(project, sources, t, order as i32));
            }
        }
        self.parts = parts;
        self.failed.retain(|id| self.slots.contains_key(&id.base()));
    }

    /// An audio source for the model (opening its file the first time).
    fn source_of(
        &mut self,
        project: &Project,
        sources: &SourceMap,
        id: AudioSourceId,
    ) -> Option<Source> {
        let s = project.sources.get(&id)?;
        let name = Some(s.name.clone());
        match &s.spec {
            SourceSpec::File { path, .. } => {
                let fresh = self.files.get(&id).is_none_or(|f| f.path != *path);
                if fresh {
                    match WavFile::open(path) {
                        Ok(file) => {
                            let open = OpenFile {
                                path: path.clone(),
                                frames: file.frames() as i64,
                                channels: file.channels() as u32,
                                rate: f64::from(file.sample_rate()),
                                samples: Arc::new(FileSamples { file }),
                            };
                            self.files.insert(id, open);
                        }
                        Err(e) => {
                            tracing::warn!("ARA: cannot open {}: {e}", path.display());
                            self.files.remove(&id);
                            return None;
                        }
                    }
                }
                let f = self.files.get(&id)?;
                Some(Source {
                    key: source_key(id),
                    name,
                    frames: f.frames,
                    sample_rate: f.rate,
                    channels: f.channels.max(1),
                    samples: Arc::clone(&f.samples),
                })
            }
            SourceSpec::Generated { .. } => match sources.get(&id)? {
                EngineSource::Memory(data) => Some(Source {
                    key: source_key(id),
                    name,
                    frames: data.frames() as i64,
                    sample_rate: f64::from(data.sample_rate()),
                    channels: data.num_channels().max(1) as u32,
                    samples: Arc::new(MemorySamples(Arc::clone(data))),
                }),
                EngineSource::Stream(_) => None,
            },
        }
    }

    fn part_of(
        &mut self,
        project: &Project,
        sources: &SourceMap,
        t: &faderframe_project::Track,
        order: i32,
    ) -> TrackPart {
        let rate = f64::from(project.sample_rate.max(1));
        let tempo = &project.timeline.tempo;
        let mut part = TrackPart {
            sequence: Some(Sequence {
                key: sequence_key(t.id),
                name: Some(t.name.clone()),
                order,
                color: Some(color(t.color)),
            }),
            ..TrackPart::default()
        };
        let mut seen_sources: HashSet<AudioSourceId> = HashSet::new();
        for clip in project.clips_of(t.id) {
            if clip.muted {
                continue;
            }
            // (source, source frame at piece start, piece start, length,
            // source span) in project-rate frames.
            let pieces: Vec<(AudioSourceId, i64, i64, i64, i64)> = match &clip.content {
                ClipContent::Audio(a) => {
                    vec![(a.source, a.source_offset, 0, a.length, a.source_span())]
                }
                ClipContent::Takes(f) => f
                    .pieces()
                    .into_iter()
                    .filter_map(|p| {
                        let take = f.takes.get(p.take)?;
                        let len = p.end - p.start;
                        Some((take.source, take.source_offset + p.start, p.start, len, len))
                    })
                    .collect(),
                ClipContent::Midi(_) => Vec::new(),
            };
            let start = tempo.musical_to_seconds(clip.start);
            for (i, (source, offset, at, length, span)) in pieces.into_iter().enumerate() {
                if length <= 0 {
                    continue;
                }
                if seen_sources.insert(source) {
                    match self.source_of(project, sources, source) {
                        Some(s) => {
                            part.modifications.push(Modification {
                                key: modification_key(t.id, source),
                                source: s.key.clone(),
                                name: None,
                            });
                            part.sources.push(s);
                        }
                        None => continue,
                    }
                }
                if !part.sources.iter().any(|s| s.key == source_key(source)) {
                    continue;
                }
                let modification = modification_key(t.id, source);
                let stretched = span != length;
                part.regions.push(Region {
                    key: region_key(clip.id, i, &modification),
                    modification,
                    sequence: sequence_key(t.id),
                    start_in_modification: offset as f64 / rate,
                    duration_in_modification: span as f64 / rate,
                    start_in_playback: start + at as f64 / rate,
                    duration_in_playback: length as f64 / rate,
                    transformation: if stretched {
                        ffi::kARAPlaybackTransformationTimestretch
                    } else {
                        ffi::kARAPlaybackTransformationNoChanges
                    },
                    name: Some(clip.name.clone()),
                    color: Some(color(clip.color.unwrap_or(t.color))),
                });
            }
        }
        part
    }

    /// The model of a document whose plugins are `plugins`.
    fn model_for(&self, plugins: &HashSet<(ProjectFormat, String)>) -> Model {
        model_of(&self.slots, &self.parts, &self.name, &self.context, plugins)
    }

    /// Whether the slot (or its cue shadow) should be bound to ARA.
    pub fn wants(&self, plugin: PluginInstanceId) -> bool {
        self.slots.contains_key(&plugin.base())
    }

    /// Whether the instance was bound (or tried and is no ARA plugin).
    pub fn has(&self, plugin: PluginInstanceId) -> bool {
        self.bound.contains_key(&plugin) || self.failed.contains(&plugin)
    }

    pub fn is_renderer(&self, plugin: PluginInstanceId) -> bool {
        self.bound
            .get(&plugin)
            .is_some_and(|b| b.renderer.extension().plays())
    }

    /// Tracks an ARA insert renders (not bypassed): their clips do not
    /// play through the clip players.
    pub fn rendered_tracks(&self) -> HashSet<TrackId> {
        self.bound
            .iter()
            .filter(|(id, b)| {
                b.renderer.extension().plays()
                    && self.slots.get(&id.base()).is_some_and(|s| !s.bypass)
            })
            .map(|(_, b)| b.track)
            .collect()
    }

    /// Bind a new instance of an ARA slot (making its document when it is
    /// the first of its plug-in), then give it its track's regions.
    pub fn bind(
        &mut self,
        plugin: PluginInstanceId,
        instance: &mut dyn PluginInstance,
    ) -> Result<(), String> {
        let Some(info) = self.slots.get(&plugin.base()).cloned() else {
            return Err("not an ARA insert".into());
        };
        let Some(factory) = instance.ara_factory() else {
            self.failed.insert(plugin);
            return Err(format!("{} is not an ARA plug-in", info.plugin.1));
        };
        let fid = factory.id();
        if !self.docs.contains_key(&fid) {
            let document = match Document::new(factory, self.name.as_deref()) {
                Ok(d) => d,
                Err(e) => {
                    self.failed.insert(plugin);
                    return Err(e.to_string());
                }
            };
            let mut doc = Doc {
                document,
                plugins: HashSet::from([info.plugin.clone()]),
                known: HashSet::new(),
            };
            let stored = self.archives.iter().find(|a| a.factory == fid);
            let now = stored.map_or(0, |a| hash(&a.data));
            doc.known.insert(now);
            // Parked state of this project wins over its saved archive.
            let parked = self.parked.remove(&fid).filter(|p| p.known.contains(&now));
            let archive = match parked {
                Some(p) => {
                    doc.known.extend(p.known);
                    if p.unsaved {
                        self.unsaved.insert(fid.clone());
                    }
                    Some(p.archive)
                }
                None => stored.and_then(|a| {
                    decode_state(&a.data).map(|data| Archive {
                        id: a.archive_id.clone(),
                        data,
                    })
                }),
            };
            let model = self.model_for(&doc.plugins);
            let report = doc.document.sync(&model, &mut [], archive.as_ref());
            if report.restored == Some(false) {
                tracing::warn!(
                    "ARA: {} did not restore the project's archive",
                    factory.name()
                );
            }
            tracing::info!(
                "ARA: document for {} ({} regions)",
                factory.name(),
                model.regions.len()
            );
            self.docs.insert(fid.clone(), doc);
        } else if let Some(doc) = self.docs.get_mut(&fid) {
            doc.plugins.insert(info.plugin.clone());
        }
        let Some(doc) = self.docs.get(&fid) else {
            return Err("no ARA document".into());
        };
        let extension = match instance.bind_ara(doc.document.controller(), Roles::ALL) {
            Ok(e) => e,
            Err(e) => {
                self.failed.insert(plugin);
                return Err(e.to_string());
            }
        };
        self.bound.insert(
            plugin,
            Bound {
                factory: fid.clone(),
                track: info.track,
                renderer: Renderer::new(extension),
            },
        );
        // The new instance is not active yet: it takes its regions now.
        self.sync_document(&fid, &mut |_| {});
        Ok(())
    }

    /// Bring one document to its model; renderers whose playback regions
    /// change are deactivated first (`deactivate`). `true`: some were.
    fn sync_document(&mut self, fid: &str, deactivate: &mut dyn FnMut(PluginInstanceId)) -> bool {
        let Some(doc) = self.docs.get_mut(fid) else {
            return false;
        };
        let model = model_of(
            &self.slots,
            &self.parts,
            &self.name,
            &self.context,
            &doc.plugins,
        );
        let mut restart = false;
        let mut assignments = Vec::new();
        for (id, b) in self.bound.iter_mut().filter(|(_, b)| b.factory == fid) {
            let regions = model.regions_of(&sequence_key(b.track));
            if b.renderer.changes(&regions) {
                deactivate(*id);
                restart = true;
            }
            let sequences: BTreeSet<String> = [sequence_key(b.track)].into();
            assignments.push(Assignment {
                renderer: &mut b.renderer,
                regions,
                sequences,
            });
        }
        doc.document.sync(&model, &mut assignments, None);
        restart
    }

    /// Bring every document to its model (see [`Self::sync_document`]).
    pub fn sync_all(&mut self, deactivate: &mut dyn FnMut(PluginInstanceId)) -> bool {
        let ids: Vec<String> = self.docs.keys().cloned().collect();
        let mut restart = false;
        for fid in ids {
            restart |= self.sync_document(&fid, deactivate);
        }
        restart
    }

    /// Documents whose project archive they do not know (another project
    /// was opened, a version restored): they are made again from it.
    pub fn replaced(&self) -> Vec<String> {
        self.docs
            .iter()
            .filter(|(fid, d)| {
                let now = self.archives.iter().find(|a| &a.factory == *fid);
                !d.known.contains(&now.map_or(0, |a| hash(&a.data)))
            })
            .map(|(fid, _)| fid.clone())
            .collect()
    }

    /// The instances bound to a document.
    pub fn bound_to(&self, fid: &str) -> Vec<PluginInstanceId> {
        self.bound
            .iter()
            .filter(|(_, b)| b.factory == fid)
            .map(|(id, _)| *id)
            .collect()
    }

    /// The instance is going (dropped by the plugin host).
    pub fn forget(&mut self, plugin: PluginInstanceId) {
        self.bound.remove(&plugin);
        self.failed.remove(&plugin);
    }

    /// Drop a document (its instances must be gone first), with its state:
    /// another project's document.
    pub fn drop_document(&mut self, fid: &str) {
        debug_assert!(self.bound_to(fid).is_empty());
        self.docs.remove(fid);
        self.unsaved.remove(fid);
        self.parked.remove(fid);
    }

    /// Parked states of another project are forgotten.
    fn forget_stale_parked(&mut self) {
        let archives = &self.archives;
        self.parked.retain(|fid, p| {
            let now = archives.iter().find(|a| &a.factory == fid);
            p.known.contains(&now.map_or(0, |a| hash(&a.data)))
        });
    }

    /// Drop documents nothing binds to and no insert of the project wants.
    pub fn drop_unused(&mut self) {
        let unused: Vec<String> = self
            .docs
            .iter()
            .filter(|(fid, d)| {
                self.bound_to(fid).is_empty()
                    && !self.slots.values().any(|s| d.plugins.contains(&s.plugin))
            })
            .map(|(fid, _)| fid.clone())
            .collect();
        for fid in unused {
            let Some(mut doc) = self.docs.remove(&fid) else {
                continue;
            };
            match doc.document.store() {
                Ok(archive) => {
                    let factory = doc.document.factory();
                    self.parked.insert(
                        fid.clone(),
                        Parked {
                            archive,
                            plugin: factory.name(),
                            vendor: factory.vendor(),
                            version: factory.version(),
                            known: std::mem::take(&mut doc.known),
                            unsaved: self.unsaved.remove(&fid),
                        },
                    );
                }
                Err(e) => tracing::warn!("ARA: {e}"),
            }
        }
        self.forget_stale_parked();
    }

    /// Let the plug-ins report; the slots of documents whose state changed
    /// (for "unsaved").
    pub fn poll(&mut self) -> Vec<PluginInstanceId> {
        let mut dirty = Vec::new();
        let ids: Vec<String> = self.docs.keys().cloned().collect();
        for fid in ids {
            let Some(doc) = self.docs.get_mut(&fid) else {
                continue;
            };
            let u = doc.document.poll();
            if u.is_empty() {
                continue;
            }
            let name = doc.document.factory().name();
            if u.changes_state() {
                self.unsaved.insert(fid.clone());
                dirty.extend(self.bound_to(&fid).into_iter().map(|id| id.base()));
            }
            self.requests.extend(u.playback);
            for a in u.analysis {
                self.analysis
                    .retain(|(n, b)| !(*n == name && b.source == a.source));
                self.analysis.push((name.clone(), a));
            }
        }
        dirty.sort();
        dirty.dedup();
        dirty
    }

    /// Whether a document changed since the archives were last captured.
    pub fn unsaved(&self) -> bool {
        !self.unsaved.is_empty() || self.parked.values().any(|p| p.unsaved)
    }

    /// Every document's archive (before saving or rendering).
    pub fn capture(&mut self) -> Vec<AraArchive> {
        let mut out = Vec::new();
        for (fid, doc) in &mut self.docs {
            let factory = doc.document.factory();
            match doc.document.store() {
                Ok(a) => {
                    let data = encode_state(&a.data);
                    doc.known.insert(hash(&data));
                    out.push(AraArchive {
                        factory: fid.clone(),
                        plugin: factory.name(),
                        vendor: factory.vendor(),
                        version: factory.version(),
                        archive_id: a.id,
                        data,
                    });
                }
                Err(e) => tracing::warn!("ARA: {e}"),
            }
        }
        for (fid, p) in &mut self.parked {
            let data = encode_state(&p.archive.data);
            p.known.insert(hash(&data));
            p.unsaved = false;
            out.push(AraArchive {
                factory: fid.clone(),
                plugin: p.plugin.clone(),
                vendor: p.vendor.clone(),
                version: p.version.clone(),
                archive_id: p.archive.id.clone(),
                data,
            });
        }
        for a in &out {
            match self.archives.iter_mut().find(|x| x.factory == a.factory) {
                Some(x) => *x = a.clone(),
                None => self.archives.push(a.clone()),
            }
        }
        self.unsaved.clear();
        out
    }

    /// The document and region keys a clip plays through on `track`.
    fn regions_of_clip(&self, clip: ClipId, track: TrackId) -> Option<(&Doc, Vec<String>)> {
        let b = self.bound.values().find(|b| b.track == track)?;
        let doc = self.docs.get(&b.factory)?;
        let prefix = region_prefix(clip);
        let keys: Vec<String> = self
            .parts
            .get(&track)?
            .regions
            .iter()
            .filter(|r| r.key.starts_with(&prefix) && doc.document.has_region(&r.key))
            .map(|r| r.key.clone())
            .collect();
        Some((doc, keys))
    }

    /// The notes the plug-in found in a clip (song time in seconds).
    pub fn notes(&self, clip: ClipId, track: TrackId) -> Option<Vec<Note>> {
        let (doc, keys) = self.regions_of_clip(clip, track)?;
        let mut out = Vec::new();
        let mut any = false;
        for k in keys {
            if let Some(n) = doc.document.region_content::<Note>(&k) {
                any = true;
                out.extend(n);
            }
        }
        out.sort_by(|a, b| a.start.total_cmp(&b.start));
        any.then_some(out)
    }

    /// Tell an ARA insert's editor what is selected: these clips (or, with
    /// none, its whole track).
    pub fn select(&self, plugin: PluginInstanceId, clips: &[ClipId]) {
        let Some(b) = self.bound.get(&plugin) else {
            return;
        };
        let Some(doc) = self.docs.get(&b.factory) else {
            return;
        };
        let mut regions = Vec::new();
        for c in clips {
            if let Some((_, keys)) = self.regions_of_clip(*c, b.track) {
                regions.extend(keys);
            }
        }
        doc.document.select(
            b.renderer.extension(),
            &regions,
            &[sequence_key(b.track)],
            None,
        );
    }

    /// Wait (polling the plug-ins) until no document is analysing, or
    /// `timeout` has passed; `false` on the timeout. Renders call it so a
    /// plug-in that is still analysing is not rendered half-done.
    pub fn settle(&mut self, timeout: std::time::Duration) -> bool {
        let end = std::time::Instant::now() + timeout;
        loop {
            self.poll();
            if !self.docs.values().any(|d| d.document.analysing()) {
                return true;
            }
            if std::time::Instant::now() >= end {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    pub fn take_requests(&mut self) -> Vec<PlaybackRequest> {
        std::mem::take(&mut self.requests)
    }

    /// The newest analysis progress per plug-in and source.
    pub fn analysis(&self) -> Vec<(String, Analysis)> {
        self.analysis.clone()
    }
}

/// What a document whose plugins are `plugins` should hold: the tracks of
/// their inserts, in the project's order.
fn model_of(
    slots: &HashMap<PluginInstanceId, SlotInfo>,
    parts: &HashMap<TrackId, TrackPart>,
    name: &Option<String>,
    context: &Context,
    plugins: &HashSet<(ProjectFormat, String)>,
) -> Model {
    let mut tracks: Vec<TrackId> = slots
        .values()
        .filter(|s| plugins.contains(&s.plugin))
        .map(|s| s.track)
        .collect();
    tracks.sort();
    tracks.dedup();
    let mut model = Model {
        name: name.clone(),
        context: context.clone(),
        ..Model::default()
    };
    for t in tracks {
        let Some(p) = parts.get(&t) else { continue };
        model.sequences.extend(p.sequence.clone());
        for s in &p.sources {
            if model.source(&s.key).is_none() {
                model.sources.push(s.clone());
            }
        }
        model.modifications.extend(p.modifications.iter().cloned());
        model.regions.extend(p.regions.iter().cloned());
    }
    model.sequences.sort_by_key(|s| s.order);
    model
}

/// The project's musical context for ARA: tempo sync points (ramps
/// sampled every quarter), bar signatures, keys and the chord track.
fn context_of(project: &Project) -> Context {
    let tempo = &project.timeline.tempo;
    let points = tempo.points();
    let mut entries = Vec::new();
    for (i, p) in points.iter().enumerate() {
        let q = p.position.quarters();
        entries.push(TempoEntry {
            seconds: tempo.musical_to_seconds(p.position),
            quarters: q,
        });
        if p.curve == faderframe_timeline::TempoCurve::Linear
            && let Some(next) = points.get(i + 1)
        {
            let end = next.position.quarters();
            let mut at = q.floor() + 1.0;
            let mut n = 0;
            while at < end && n < 4096 {
                let pos = faderframe_timeline::MusicalTime::from_quarters(at);
                entries.push(TempoEntry {
                    seconds: tempo.musical_to_seconds(pos),
                    quarters: at,
                });
                at += 1.0;
                n += 1;
            }
        }
    }
    let first_bpm = points.first().map_or(120.0, |p| p.bpm);
    let tempo_entries = Context::tempo_entries(&entries, 60.0 / first_bpm.max(1.0));
    let meter = &project.timeline.meter;
    let mut bars: Vec<BarSignature> = vec![{
        let s = meter.signature_at(faderframe_timeline::MusicalTime::ZERO);
        BarSignature {
            numerator: i32::from(s.numerator),
            denominator: i32::from(s.denominator),
            quarters: OrderedQuarters::new(0.0),
        }
    }];
    for c in meter.changes() {
        let q = meter.bar_start(c.bar).quarters();
        if q <= 0.0 {
            continue;
        }
        bars.push(BarSignature {
            numerator: i32::from(c.signature.numerator),
            denominator: i32::from(c.signature.denominator),
            quarters: OrderedQuarters::new(q),
        });
    }
    let keys = project
        .keys
        .iter()
        .map(|k| {
            let mut intervals = [false; 12];
            for i in k.key.scale.intervals() {
                intervals[usize::from(*i % 12)] = true;
            }
            KeySignature {
                root: circle_of_fifths(k.key.root),
                intervals,
                name: None,
                quarters: OrderedQuarters::new(k.at.quarters()),
            }
        })
        .collect();
    let mut chords: Vec<Chord> = Vec::new();
    for (i, c) in project.chords.iter().enumerate() {
        let mut intervals = [0u8; 12];
        for iv in c.chord.quality.intervals() {
            intervals[usize::from(*iv % 12)] = ffi::kARAChordIntervalUsed;
        }
        let root = circle_of_fifths(c.chord.root);
        chords.push(Chord {
            root,
            bass: c.chord.bass.map_or(root, circle_of_fifths),
            intervals,
            name: None,
            quarters: OrderedQuarters::new(c.start.quarters()),
        });
        // A gap before the next chord: ARA's "no chord".
        let next = project.chords.get(i + 1).map(|n| n.start);
        if next.is_none_or(|n| n > c.end) {
            chords.push(Chord {
                root: 0,
                bass: 0,
                intervals: [0; 12],
                name: None,
                quarters: OrderedQuarters::new(c.end.quarters()),
            });
        }
    }
    Context {
        tempo: tempo_entries,
        bars,
        keys,
        chords,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_musical_context_follows_the_project() {
        let mut p = Project::new("p", 48_000);
        p.timeline.tempo.set_initial_bpm(90.0);
        let c = context_of(&p);
        assert_eq!(c.tempo.len(), 2);
        assert_eq!(c.tempo[0].quarters, 0.0);
        assert!((c.tempo[1].seconds - 60.0 / 90.0).abs() < 1e-9);
        assert_eq!(c.bars.len(), 1);
        assert_eq!(c.bars[0].numerator, 4);
        p.keys.push(faderframe_project::KeyChange {
            at: faderframe_timeline::MusicalTime::ZERO,
            key: faderframe_project::harmony::Key::new(
                7,
                faderframe_project::harmony::Scale::Major,
            ),
        });
        p.chords.push(faderframe_project::ChordEvent {
            start: faderframe_timeline::MusicalTime::ZERO,
            end: faderframe_timeline::MusicalTime::from_quarters(4.0),
            chord: faderframe_project::harmony::Chord::new(
                9,
                faderframe_project::harmony::Quality::Minor,
            ),
        });
        let c = context_of(&p);
        assert_eq!(c.keys[0].root, 1, "G");
        assert!(c.keys[0].intervals[4] && !c.keys[0].intervals[3]);
        assert_eq!(c.chords.len(), 2, "the chord and the gap after it");
        assert_eq!(c.chords[0].root, 3, "A");
        assert_eq!(c.chords[0].intervals[3], ffi::kARAChordIntervalUsed);
        assert_eq!(c.chords[1].intervals, [0; 12]);
    }
}
