//! One ARA document: the host's interfaces and the plug-in's document
//! controller, with a mirror of what the plug-in's model graph holds.

use crate::content::PlugInContent;
use crate::host::{ArchiveIn, ArchiveOut, HostCore, HostKey, SourceHost};
use crate::model::{Assignment, Model, Modification, Region, Sequence, Source};
use crate::{AraError, Factory, ffi};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{CString, c_void};
use std::mem::offset_of;
use std::sync::{Arc, Mutex};

/// The document controller's reference, for binding companion instances.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ControllerRef(ffi::DocumentControllerRef);

impl ControllerRef {
    pub fn as_raw(self) -> *mut c_void {
        self.0
    }

    /// A reference a companion API handed over (a plug-in side receiving
    /// the host's binding call, as in tests).
    pub fn from_raw(raw: *mut c_void) -> Self {
        Self(raw)
    }
}

/// An archive of the plug-in's document state and the archive ID it was
/// written under.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Archive {
    pub id: String,
    pub data: Vec<u8>,
}

/// What a [`Document::sync`] did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SyncReport {
    /// An edit cycle ran (something changed).
    pub edited: bool,
    /// An archive was given: whether the plug-in restored it.
    pub restored: Option<bool>,
}

/// How far a source's analysis is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnalysisState {
    Started,
    Updated,
    Completed,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Analysis {
    pub source: String,
    pub state: AnalysisState,
    pub progress: f32,
}

/// Something the plug-in asked the host's transport for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PlaybackRequest {
    Start,
    Stop,
    /// Song time in seconds.
    Locate(f64),
    CycleRange {
        start: f64,
        duration: f64,
    },
    EnableCycle(bool),
}

/// What the plug-in reported since the last [`Document::poll`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Updates {
    /// The newest progress per source.
    pub analysis: Vec<Analysis>,
    /// Sources whose content (analysis) changed, by key.
    pub sources: Vec<String>,
    /// Modifications the user edited in the plug-in, by key.
    pub modifications: Vec<String>,
    /// Regions whose rendered content changed, by key.
    pub regions: Vec<String>,
    /// Private document data changed.
    pub document: bool,
    pub playback: Vec<PlaybackRequest>,
}

impl Updates {
    /// The document's archive is no longer what was saved.
    pub fn changes_state(&self) -> bool {
        !self.sources.is_empty() || !self.modifications.is_empty() || self.document
    }

    pub fn is_empty(&self) -> bool {
        self.analysis.is_empty()
            && !self.changes_state()
            && self.regions.is_empty()
            && self.playback.is_empty()
    }
}

struct SeqEntry {
    r: ffi::RegionSequenceRef,
    desc: Sequence,
    /// The plug-in's host reference (kept alive, never read here).
    _host: Box<HostKey>,
}

struct SrcEntry {
    r: ffi::AudioSourceRef,
    desc: Source,
    host: Box<SourceHost>,
    active: bool,
}

struct ModEntry {
    r: ffi::AudioModificationRef,
    desc: Modification,
    _host: Box<HostKey>,
    active: bool,
}

struct RegEntry {
    r: ffi::PlaybackRegionRef,
    desc: Region,
    _host: Box<HostKey>,
}

/// The objects the plug-in holds, by key.
#[derive(Default)]
struct Mirror {
    name: Option<String>,
    context: Option<ffi::MusicalContextRef>,
    sequences: BTreeMap<String, SeqEntry>,
    sources: BTreeMap<String, SrcEntry>,
    modifications: BTreeMap<String, ModEntry>,
    regions: BTreeMap<String, RegEntry>,
}

/// An ARA document and the plug-in's controller for it. Lives on the
/// control thread; dropping it destroys the plug-in's model graph and its
/// controller (companion instances bound to it may die before or after).
pub struct Document {
    factory: Factory,
    core: Box<HostCore>,
    controller: ffi::DocumentControllerRef,
    api: ffi::DocumentControllerInterface,
    mirror: Mirror,
    /// Inside an edit cycle.
    editing: bool,
}

/// A C string for the plug-in (NUL bytes dropped).
fn cstring(s: &str) -> CString {
    CString::new(s.replace('\0', "")).unwrap_or_default()
}

fn opt_cstring(s: &Option<String>) -> Option<CString> {
    s.as_deref().map(cstring)
}

fn ptr_of(s: &Option<CString>) -> *const std::ffi::c_char {
    s.as_ref().map_or(std::ptr::null(), |c| c.as_ptr())
}

fn color_of(c: Option<[f32; 3]>) -> Option<ffi::Color> {
    c.map(|[r, g, b]| ffi::Color { r, g, b })
}

fn color_ptr(c: &Option<ffi::Color>) -> *const ffi::Color {
    c.as_ref()
        .map_or(std::ptr::null(), |c| c as *const ffi::Color)
}

impl Document {
    /// Initialise the factory (once per process) and have it make a
    /// document controller.
    pub fn new(factory: Factory, name: Option<&str>) -> Result<Document, AraError> {
        factory.initialize()?;
        let core = HostCore::new();
        let name_c = name.map(cstring);
        let props = ffi::DocumentProperties {
            structSize: std::mem::size_of::<ffi::DocumentProperties>(),
            name: name_c.as_ref().map_or(std::ptr::null(), |c| c.as_ptr()),
        };
        let create = factory
            .raw()
            .createDocumentControllerWithDocument
            .ok_or_else(|| AraError::InvalidFactory(factory.name()))?;
        // SAFETY: the host instance lives in a box this document owns until
        // after the controller is destroyed; the properties are valid for
        // the call.
        let instance = unsafe { create(&core.instance, &props) };
        if instance.is_null() {
            return Err(AraError::NoController(factory.name()));
        }
        // SAFETY: the plug-in's instance struct, valid until the controller
        // is destroyed (copied: packed, any alignment).
        let instance = unsafe { instance.read_unaligned() };
        let api_ptr = instance.documentControllerInterface;
        if instance.structSize
            < offset_of!(ffi::DocumentControllerInstance, documentControllerInterface)
                + std::mem::size_of::<usize>()
            || api_ptr.is_null()
        {
            return Err(AraError::NoController(factory.name()));
        }
        // SAFETY: the plug-in's interface table, valid as long as the
        // controller; fields past its size are cleared below.
        let mut api = unsafe { api_ptr.read_unaligned() };
        Self::clear_missing(&mut api);
        let controller = instance.documentControllerRef;
        let doc = Document {
            factory,
            core,
            controller,
            api,
            mirror: Mirror {
                name: name.map(str::to_string),
                ..Mirror::default()
            },
            editing: false,
        };
        let a = &doc.api;
        let required = a.destroyDocumentController.is_some()
            && a.beginEditing.is_some()
            && a.endEditing.is_some()
            && a.notifyModelUpdates.is_some()
            && a.createMusicalContext.is_some()
            && a.createAudioSource.is_some()
            && a.enableAudioSourceSamplesAccess.is_some()
            && a.createAudioModification.is_some()
            && a.createPlaybackRegion.is_some()
            && a.createRegionSequence.is_some()
            && a.storeObjectsToArchive.is_some()
            && a.restoreObjectsFromArchive.is_some();
        if !required {
            // Dropping destroys the controller.
            return Err(AraError::Incomplete(doc.factory.name()));
        }
        Ok(doc)
    }

    /// Entries past the plug-in's `structSize` are not its functions.
    fn clear_missing(api: &mut ffi::DocumentControllerInterface) {
        let size = api.structSize;
        macro_rules! keep {
            ($($f:ident),*) => {$(
                if !ffi::implements(size, offset_of!(ffi::DocumentControllerInterface, $f)) {
                    api.$f = None;
                }
            )*};
        }
        keep!(
            createRegionSequence,
            updateRegionSequenceProperties,
            destroyRegionSequence,
            getPlaybackRegionHeadAndTailTime,
            restoreObjectsFromArchive,
            storeObjectsToArchive,
            getProcessingAlgorithmsCount,
            getProcessingAlgorithmProperties,
            getProcessingAlgorithmForAudioSource,
            requestProcessingAlgorithmForAudioSource,
            isLicensedForCapabilities,
            storeAudioSourceToAudioFileChunk,
            isAudioModificationPreservingAudioSourceSignal
        );
    }

    pub fn factory(&self) -> Factory {
        self.factory
    }

    /// For binding companion instances (VST3 `IPlugInEntryPoint2`, CLAP's
    /// ARA plug-in extension).
    pub fn controller(&self) -> ControllerRef {
        ControllerRef(self.controller)
    }

    fn begin(&mut self) {
        if let Some(f) = self.api.beginEditing {
            // SAFETY: a live controller, control thread.
            unsafe { f(self.controller) };
        }
        self.editing = true;
    }

    fn end(&mut self) {
        if let Some(f) = self.api.endEditing {
            // SAFETY: as above.
            unsafe { f(self.controller) };
        }
        self.editing = false;
    }

    /// Whether the plug-in's graph differs from `model` (or the renderers
    /// from their assignments).
    pub fn differs(&self, model: &Model, assignments: &[Assignment]) -> bool {
        let m = &self.mirror;
        if m.context.is_none()
            || m.name != model.name
            || self
                .core
                .context
                .lock()
                .map(|c| *c != model.context)
                .unwrap_or(true)
        {
            return true;
        }
        let same_keys = |a: Vec<&String>, b: Vec<&String>| {
            a.into_iter().collect::<BTreeSet<_>>() == b.into_iter().collect::<BTreeSet<_>>()
        };
        let active_sources: Vec<&String> = m
            .sources
            .iter()
            .filter(|(_, e)| e.active)
            .map(|(k, _)| k)
            .collect();
        let active_mods: Vec<&String> = m
            .modifications
            .iter()
            .filter(|(_, e)| e.active)
            .map(|(k, _)| k)
            .collect();
        if !same_keys(
            m.sequences.keys().collect(),
            model.sequences.iter().map(|s| &s.key).collect(),
        ) || !same_keys(
            active_sources,
            model.sources.iter().map(|s| &s.key).collect(),
        ) || !same_keys(
            active_mods,
            model.modifications.iter().map(|s| &s.key).collect(),
        ) || !same_keys(
            m.regions.keys().collect(),
            model.regions.iter().map(|s| &s.key).collect(),
        ) {
            return true;
        }
        model
            .sequences
            .iter()
            .any(|s| m.sequences.get(&s.key).is_some_and(|e| e.desc != *s))
            || model.sources.iter().any(|s| {
                m.sources.get(&s.key).is_some_and(|e| {
                    !e.desc.same_shape(s) || !Arc::ptr_eq(&e.desc.samples, &s.samples)
                })
            })
            || model
                .modifications
                .iter()
                .any(|s| m.modifications.get(&s.key).is_some_and(|e| e.desc != *s))
            || model
                .regions
                .iter()
                .any(|s| m.regions.get(&s.key).is_some_and(|e| e.desc != *s))
            || assignments.iter().any(|a| {
                (a.renderer.extension().plays() && a.renderer.regions != a.regions)
                    || (a.renderer.extension().previews() && a.renderer.sequences != a.sequences)
            })
    }

    /// Bring the plug-in's graph to `model` in one edit cycle, restore
    /// `archive` into it (once all its objects exist) and give each
    /// renderer its regions.
    ///
    /// Objects the model no longer has: regions and sequences are
    /// destroyed; modifications and sources are deactivated for the undo
    /// history and come back with their edits when the model has them
    /// again (an undone delete). A renderer whose playback regions change
    /// must belong to an inactive instance ([`crate::Renderer::changes`]).
    pub fn sync(
        &mut self,
        model: &Model,
        assignments: &mut [Assignment],
        archive: Option<&Archive>,
    ) -> SyncReport {
        let mut report = SyncReport::default();
        if archive.is_none() && !self.differs(model, assignments) {
            return report;
        }
        debug_assert!(model.is_consistent(), "inconsistent ARA model");
        report.edited = true;
        self.begin();
        self.sync_document(model);
        self.sync_context(model);
        self.sync_sequences(model);
        self.sync_sources(model);
        self.sync_modifications(model);
        self.sync_regions(model);
        if let Some(a) = archive {
            report.restored = Some(self.restore(a));
        }
        self.assign(model, assignments);
        self.retire(model);
        self.end();
        report
    }

    fn sync_document(&mut self, model: &Model) {
        if self.mirror.name == model.name {
            return;
        }
        let name = opt_cstring(&model.name);
        let props = ffi::DocumentProperties {
            structSize: std::mem::size_of::<ffi::DocumentProperties>(),
            name: ptr_of(&name),
        };
        if let Some(f) = self.api.updateDocumentProperties {
            // SAFETY: inside an edit cycle; properties valid for the call.
            unsafe { f(self.controller, &props) };
        }
        self.mirror.name = model.name.clone();
    }

    fn context_props(&self) -> ffi::MusicalContextProperties {
        ffi::MusicalContextProperties {
            structSize: std::mem::size_of::<ffi::MusicalContextProperties>(),
            name: std::ptr::null(),
            orderIndex: 0,
            color: std::ptr::null(),
        }
    }

    fn sync_context(&mut self, model: &Model) {
        let changed = self
            .core
            .context
            .lock()
            .map(|mut c| {
                let changed = *c != model.context;
                *c = model.context.clone();
                changed
            })
            .unwrap_or(false);
        match self.mirror.context {
            None => {
                let props = self.context_props();
                if let Some(f) = self.api.createMusicalContext {
                    // SAFETY: inside an edit cycle; the host reference is
                    // the core, alive as long as the document.
                    let r = unsafe { f(self.controller, self.core.context_ref(), &props) };
                    self.mirror.context = Some(r);
                }
            }
            Some(r) if changed => {
                if let Some(f) = self.api.updateMusicalContextContent {
                    // SAFETY: as above; a null range = everything.
                    unsafe {
                        f(
                            self.controller,
                            r,
                            std::ptr::null(),
                            ffi::kARAContentUpdateEverythingChanged,
                        )
                    };
                }
            }
            Some(_) => {}
        }
    }

    fn sequence_props(
        &self,
        s: &Sequence,
        name: &Option<CString>,
        color: &Option<ffi::Color>,
    ) -> ffi::RegionSequenceProperties {
        ffi::RegionSequenceProperties {
            structSize: std::mem::size_of::<ffi::RegionSequenceProperties>(),
            name: ptr_of(name),
            orderIndex: s.order,
            musicalContextRef: self.mirror.context.unwrap_or(std::ptr::null_mut()),
            color: color_ptr(color),
        }
    }

    fn sync_sequences(&mut self, model: &Model) {
        for s in &model.sequences {
            let name = opt_cstring(&s.name);
            let color = color_of(s.color);
            let props = self.sequence_props(s, &name, &color);
            match self.mirror.sequences.get_mut(&s.key) {
                Some(e) if e.desc != *s => {
                    if let Some(f) = self.api.updateRegionSequenceProperties {
                        // SAFETY: inside an edit cycle; a live sequence.
                        unsafe { f(self.controller, e.r, &props) };
                    }
                    e.desc = s.clone();
                }
                Some(_) => {}
                None => {
                    let host = Box::new(HostKey { key: s.key.clone() });
                    let Some(f) = self.api.createRegionSequence else {
                        continue;
                    };
                    // SAFETY: inside an edit cycle; the host key lives in
                    // the mirror until after the sequence is destroyed.
                    let r = unsafe {
                        f(
                            self.controller,
                            &*host as *const HostKey as *mut c_void,
                            &props,
                        )
                    };
                    self.mirror.sequences.insert(
                        s.key.clone(),
                        SeqEntry {
                            r,
                            desc: s.clone(),
                            _host: host,
                        },
                    );
                }
            }
        }
    }

    fn source_props(
        s: &Source,
        name: &Option<CString>,
        id: &CString,
    ) -> ffi::AudioSourceProperties {
        ffi::AudioSourceProperties {
            structSize: std::mem::size_of::<ffi::AudioSourceProperties>(),
            name: ptr_of(name),
            persistentID: id.as_ptr(),
            sampleCount: s.frames.max(0),
            sampleRate: s.sample_rate,
            channelCount: s.channels.max(1) as i32,
            merits64BitSamples: ffi::FALSE,
            channelArrangementDataType: ffi::kARAChannelArrangementUndefined,
            channelArrangement: std::ptr::null(),
        }
    }

    fn enable_access(&self, r: ffi::AudioSourceRef, on: bool) {
        if let Some(f) = self.api.enableAudioSourceSamplesAccess {
            // SAFETY: a live source; allowed inside or outside a cycle.
            unsafe { f(self.controller, r, if on { ffi::TRUE } else { ffi::FALSE }) };
        }
    }

    fn sync_sources(&mut self, model: &Model) {
        for s in &model.sources {
            let name = opt_cstring(&s.name);
            let id = cstring(&s.key);
            let props = Self::source_props(s, &name, &id);
            if let Some(e) = self.mirror.sources.get_mut(&s.key) {
                let (r, was_active) = (e.r, e.active);
                // The samples move with the file; readers made from now on
                // read the new one.
                if let Ok(mut samples) = e.host.samples.lock() {
                    *samples = Arc::clone(&s.samples);
                }
                let reshaped = !e.desc.same_shape(s);
                e.desc = s.clone();
                e.active = true;
                if !was_active && let Some(f) = self.api.deactivateAudioSourceForUndoHistory {
                    // SAFETY: inside an edit cycle; a live, deactivated
                    // source, re-activated before its modifications.
                    unsafe { f(self.controller, r, ffi::FALSE) };
                }
                if reshaped {
                    // Count, rate and channels change only without access.
                    self.enable_access(r, false);
                    if let Some(e) = self.mirror.sources.get_mut(&s.key) {
                        e.host.channels = s.channels.max(1) as usize;
                    }
                    if let Some(f) = self.api.updateAudioSourceProperties {
                        // SAFETY: inside an edit cycle; a live source.
                        unsafe { f(self.controller, r, &props) };
                    }
                    self.enable_access(r, true);
                }
                continue;
            }
            let host = Box::new(SourceHost {
                key: s.key.clone(),
                channels: s.channels.max(1) as usize,
                samples: Mutex::new(Arc::clone(&s.samples)),
            });
            let Some(f) = self.api.createAudioSource else {
                continue;
            };
            // SAFETY: inside an edit cycle; the host box lives in the
            // mirror until after the source is destroyed.
            let r = unsafe {
                f(
                    self.controller,
                    &*host as *const SourceHost as *mut c_void,
                    &props,
                )
            };
            self.enable_access(r, true);
            self.mirror.sources.insert(
                s.key.clone(),
                SrcEntry {
                    r,
                    desc: s.clone(),
                    host,
                    active: true,
                },
            );
        }
    }

    fn sync_modifications(&mut self, model: &Model) {
        for m in &model.modifications {
            let name = opt_cstring(&m.name);
            let id = cstring(&m.key);
            let props = ffi::AudioModificationProperties {
                structSize: std::mem::size_of::<ffi::AudioModificationProperties>(),
                name: ptr_of(&name),
                persistentID: id.as_ptr(),
            };
            if let Some(e) = self.mirror.modifications.get_mut(&m.key) {
                if !e.active {
                    if let Some(f) = self.api.deactivateAudioModificationForUndoHistory {
                        // SAFETY: inside an edit cycle; its source was
                        // re-activated first.
                        unsafe { f(self.controller, e.r, ffi::FALSE) };
                    }
                    e.active = true;
                }
                if e.desc != *m {
                    if e.desc.source != m.source {
                        tracing::warn!("ARA: modification {} cannot move to another source", m.key);
                    } else if let Some(f) = self.api.updateAudioModificationProperties {
                        // SAFETY: inside an edit cycle; a live modification.
                        unsafe { f(self.controller, e.r, &props) };
                    }
                    e.desc.name = m.name.clone();
                }
                continue;
            }
            let Some(source) = self.mirror.sources.get(&m.source).map(|e| e.r) else {
                continue;
            };
            let host = Box::new(HostKey { key: m.key.clone() });
            let Some(f) = self.api.createAudioModification else {
                continue;
            };
            // SAFETY: inside an edit cycle; a live source; the host key
            // lives in the mirror until after the modification goes.
            let r = unsafe {
                f(
                    self.controller,
                    source,
                    &*host as *const HostKey as *mut c_void,
                    &props,
                )
            };
            self.mirror.modifications.insert(
                m.key.clone(),
                ModEntry {
                    r,
                    desc: m.clone(),
                    _host: host,
                    active: true,
                },
            );
        }
    }

    fn region_props(
        &self,
        r: &Region,
        name: &Option<CString>,
        color: &Option<ffi::Color>,
    ) -> Option<ffi::PlaybackRegionProperties> {
        let sequence = self.mirror.sequences.get(&r.sequence)?.r;
        // Only transformations the plug-in supports; without stretching the
        // modification's duration is the playback duration.
        let flags = r.transformation & self.factory.transformations();
        let duration_in_modification = if flags & ffi::kARAPlaybackTransformationTimestretch != 0 {
            r.duration_in_modification
        } else {
            r.duration_in_playback
        };
        Some(ffi::PlaybackRegionProperties {
            structSize: std::mem::size_of::<ffi::PlaybackRegionProperties>(),
            transformationFlags: flags,
            startInModificationTime: r.start_in_modification,
            durationInModificationTime: duration_in_modification,
            startInPlaybackTime: r.start_in_playback,
            durationInPlaybackTime: r.duration_in_playback,
            musicalContextRef: self.mirror.context.unwrap_or(std::ptr::null_mut()),
            regionSequenceRef: sequence,
            name: ptr_of(name),
            color: color_ptr(color),
        })
    }

    fn sync_regions(&mut self, model: &Model) {
        for r in &model.regions {
            let name = opt_cstring(&r.name);
            let color = color_of(r.color);
            let Some(props) = self.region_props(r, &name, &color) else {
                continue;
            };
            if let Some(e) = self.mirror.regions.get_mut(&r.key) {
                if e.desc != *r {
                    if e.desc.modification != r.modification {
                        tracing::warn!("ARA: region {} cannot change its modification", r.key);
                    } else if let Some(f) = self.api.updatePlaybackRegionProperties {
                        // SAFETY: inside an edit cycle; a live region.
                        unsafe { f(self.controller, e.r, &props) };
                        e.desc = r.clone();
                    }
                }
                continue;
            }
            let Some(modification) = self
                .mirror
                .modifications
                .get(&r.modification)
                .filter(|e| e.active)
                .map(|e| e.r)
            else {
                continue;
            };
            let host = Box::new(HostKey { key: r.key.clone() });
            let Some(f) = self.api.createPlaybackRegion else {
                continue;
            };
            // SAFETY: inside an edit cycle; a live modification; the host
            // key lives in the mirror until after the region goes.
            let made = unsafe {
                f(
                    self.controller,
                    modification,
                    &*host as *const HostKey as *mut c_void,
                    &props,
                )
            };
            self.mirror.regions.insert(
                r.key.clone(),
                RegEntry {
                    r: made,
                    desc: r.clone(),
                    _host: host,
                },
            );
        }
    }

    /// Give each renderer what it should render: removals first (every
    /// region about to be destroyed goes from every renderer), then
    /// additions.
    fn assign(&mut self, model: &Model, assignments: &mut [Assignment]) {
        let keep = |k: &String| model.region(k).is_some();
        for a in assignments.iter_mut() {
            let ext = *a.renderer.extension();
            let wanted: BTreeSet<String> = a.regions.iter().filter(|k| keep(k)).cloned().collect();
            let gone: Vec<String> = a.renderer.regions.difference(&wanted).cloned().collect();
            for k in gone {
                if let Some(e) = self.mirror.regions.get(&k) {
                    ext.remove_playback_region(e.r);
                }
                a.renderer.regions.remove(&k);
            }
            let sequences: BTreeSet<String> = a
                .sequences
                .iter()
                .filter(|k| model.sequences.iter().any(|s| &s.key == *k))
                .cloned()
                .collect();
            let gone: Vec<String> = a
                .renderer
                .sequences
                .difference(&sequences)
                .cloned()
                .collect();
            for k in gone {
                if let Some(e) = self.mirror.sequences.get(&k) {
                    ext.remove_sequence(e.r);
                }
                a.renderer.sequences.remove(&k);
            }
            if ext.plays() {
                for k in &wanted {
                    if !a.renderer.regions.contains(k)
                        && let Some(e) = self.mirror.regions.get(k)
                    {
                        ext.add_playback_region(e.r);
                        a.renderer.regions.insert(k.clone());
                    }
                }
            }
            if ext.previews() {
                for k in &sequences {
                    if !a.renderer.sequences.contains(k)
                        && let Some(e) = self.mirror.sequences.get(k)
                    {
                        ext.add_sequence(e.r);
                        a.renderer.sequences.insert(k.clone());
                    }
                }
            }
        }
    }

    /// Destroy regions and sequences the model no longer has; deactivate
    /// modifications and sources (kept for the undo history).
    fn retire(&mut self, model: &Model) {
        let gone: Vec<String> = self
            .mirror
            .regions
            .keys()
            .filter(|k| model.region(k).is_none())
            .cloned()
            .collect();
        for k in gone {
            if let Some(e) = self.mirror.regions.remove(&k)
                && let Some(f) = self.api.destroyPlaybackRegion
            {
                // SAFETY: inside an edit cycle; no renderer refers to it
                // (removed in `assign`).
                unsafe { f(self.controller, e.r) };
            }
        }
        for (k, e) in self.mirror.modifications.iter_mut() {
            if e.active && !model.modifications.iter().any(|m| &m.key == k) {
                if let Some(f) = self.api.deactivateAudioModificationForUndoHistory {
                    // SAFETY: inside an edit cycle; its regions are gone.
                    unsafe { f(self.controller, e.r, ffi::TRUE) };
                }
                e.active = false;
            }
        }
        for (k, e) in self.mirror.sources.iter_mut() {
            if e.active && model.source(k).is_none() {
                if let Some(f) = self.api.deactivateAudioSourceForUndoHistory {
                    // SAFETY: inside an edit cycle; its modifications were
                    // deactivated just before.
                    unsafe { f(self.controller, e.r, ffi::TRUE) };
                }
                e.active = false;
            }
        }
        let gone: Vec<String> = self
            .mirror
            .sequences
            .keys()
            .filter(|k| !model.sequences.iter().any(|s| &s.key == *k))
            .cloned()
            .collect();
        for k in gone {
            if let Some(e) = self.mirror.sequences.remove(&k)
                && let Some(f) = self.api.destroyRegionSequence
            {
                // SAFETY: inside an edit cycle; no region refers to it.
                unsafe { f(self.controller, e.r) };
            }
        }
    }

    /// Restore an archive into the objects made so far (inside the cycle).
    fn restore(&mut self, archive: &Archive) -> bool {
        if !self.factory.reads_archive(&archive.id) {
            tracing::warn!(
                "ARA: {} cannot read archives of {} (it writes {})",
                self.factory.name(),
                archive.id,
                self.factory.archive_id()
            );
            return false;
        }
        let Some(f) = self.api.restoreObjectsFromArchive else {
            return false;
        };
        let input = ArchiveIn {
            data: archive.data.clone(),
            id: cstring(&archive.id),
        };
        // SAFETY: inside an edit cycle; the archive lives for the call; no
        // filter: every archived object with a matching persistent ID.
        let ok = unsafe {
            f(
                self.controller,
                &input as *const ArchiveIn as *mut c_void,
                std::ptr::null(),
            )
        };
        ok != ffi::FALSE
    }

    /// The document's state as an archive: its active sources and
    /// modifications and its private data (what an undone delete brought
    /// back is in it again).
    pub fn store(&mut self) -> Result<Archive, AraError> {
        if self.editing {
            return Err(AraError::Store(self.factory.name()));
        }
        let Some(f) = self.api.storeObjectsToArchive else {
            return Err(AraError::Incomplete(self.factory.name()));
        };
        let sources: Vec<ffi::AudioSourceRef> = self
            .mirror
            .sources
            .values()
            .filter(|e| e.active)
            .map(|e| e.r)
            .collect();
        let modifications: Vec<ffi::AudioModificationRef> = self
            .mirror
            .modifications
            .values()
            .filter(|e| e.active)
            .map(|e| e.r)
            .collect();
        let filter = ffi::StoreObjectsFilter {
            structSize: std::mem::size_of::<ffi::StoreObjectsFilter>(),
            documentData: ffi::TRUE,
            audioSourceRefsCount: sources.len(),
            audioSourceRefs: if sources.is_empty() {
                std::ptr::null()
            } else {
                sources.as_ptr()
            },
            audioModificationRefsCount: modifications.len(),
            audioModificationRefs: if modifications.is_empty() {
                std::ptr::null()
            } else {
                modifications.as_ptr()
            },
        };
        let out = ArchiveOut {
            data: Mutex::new(Vec::new()),
        };
        // SAFETY: not editing; the writer and filter live for the call.
        let ok = unsafe {
            f(
                self.controller,
                &out as *const ArchiveOut as *mut c_void,
                &filter,
            )
        };
        if ok == ffi::FALSE {
            return Err(AraError::Store(self.factory.name()));
        }
        let data = out
            .data
            .into_inner()
            .map_err(|_| AraError::Store(self.factory.name()))?;
        Ok(Archive {
            id: self.factory.archive_id(),
            data,
        })
    }

    /// Let the plug-in report (analysis progress, content changes, playback
    /// requests) and take what it reported.
    pub fn poll(&mut self) -> Updates {
        if !self.editing
            && let Some(f) = self.api.notifyModelUpdates
        {
            // SAFETY: a live controller, not editing, control thread.
            unsafe { f(self.controller) };
        }
        self.core
            .updates
            .lock()
            .map(|mut u| std::mem::take(&mut *u))
            .unwrap_or_default()
    }

    /// Tell an editor view what the host has selected (regions first by
    /// importance, then sequences; a time range for range selections).
    pub fn select(
        &self,
        extension: &crate::Extension,
        regions: &[String],
        sequences: &[String],
        range: Option<(f64, f64)>,
    ) {
        let regions: Vec<ffi::PlaybackRegionRef> = regions
            .iter()
            .filter_map(|k| self.mirror.regions.get(k).map(|e| e.r))
            .collect();
        let sequences: Vec<ffi::RegionSequenceRef> = sequences
            .iter()
            .filter_map(|k| self.mirror.sequences.get(k).map(|e| e.r))
            .collect();
        extension.notify_selection(&regions, &sequences, range);
    }

    /// Whether the region exists in the plug-in's graph.
    pub fn has_region(&self, key: &str) -> bool {
        self.mirror.regions.contains_key(key)
    }

    /// The modifications the plug-in holds, active or not.
    pub fn modification_keys(&self) -> Vec<(String, bool)> {
        self.mirror
            .modifications
            .iter()
            .map(|(k, e)| (k.clone(), e.active))
            .collect()
    }

    /// A region's head and tail time (seconds the plug-in renders before
    /// and after it).
    pub fn head_and_tail(&self, region: &str) -> Option<(f64, f64)> {
        let r = self.mirror.regions.get(region)?.r;
        let f = self.api.getPlaybackRegionHeadAndTailTime?;
        let (mut head, mut tail) = (0.0, 0.0);
        // SAFETY: a live region; out pointers valid for the call.
        unsafe { f(self.controller, r, &mut head, &mut tail) };
        Some((head, tail))
    }

    /// Whether the plug-in is still analysing any active source (for what
    /// its factory analyses without its UI): renders wait for it.
    pub fn analysing(&self) -> bool {
        if self.editing {
            return false;
        }
        let Some(f) = self.api.isAudioSourceContentAnalysisIncomplete else {
            return false;
        };
        let types = self.factory.analyzable();
        self.mirror.sources.values().filter(|e| e.active).any(|e| {
            types.iter().any(|t| {
                // SAFETY: a live source, not editing, control thread.
                unsafe { f(self.controller, e.r, *t) != ffi::FALSE }
            })
        })
    }

    /// Ask for an analysis of a source's content (what the factory can
    /// analyse; the plug-in reports through [`Self::poll`]).
    pub fn request_analysis(&self, source: &str, types: &[ffi::ContentType]) {
        let Some(e) = self.mirror.sources.get(source).filter(|e| e.active) else {
            return;
        };
        let can = self.factory.analyzable();
        let types: Vec<ffi::ContentType> =
            types.iter().copied().filter(|t| can.contains(t)).collect();
        if types.is_empty() || self.editing {
            return;
        }
        if let Some(f) = self.api.requestAudioSourceContentAnalysis {
            // SAFETY: a live source; the types are valid for the call.
            unsafe { f(self.controller, e.r, types.len(), types.as_ptr()) };
        }
    }

    /// Read content of the type `T` through a reader, if available.
    fn read<T: PlugInContent>(
        &self,
        available: Option<
            unsafe extern "C" fn(
                ffi::DocumentControllerRef,
                *mut c_void,
                ffi::ContentType,
            ) -> ffi::Bool,
        >,
        create: Option<
            unsafe extern "C" fn(
                ffi::DocumentControllerRef,
                *mut c_void,
                ffi::ContentType,
                *const ffi::ContentTimeRange,
            ) -> ffi::ContentReaderRef,
        >,
        object: *mut c_void,
    ) -> Option<Vec<T>> {
        let (available, create) = (available?, create?);
        let count = self.api.getContentReaderEventCount?;
        let event = self.api.getContentReaderDataForEvent?;
        let destroy = self.api.destroyContentReader?;
        // SAFETY: a live object of this document; content calls in one
        // series with no other controller call in between (ARA's rule).
        unsafe {
            if available(self.controller, object, T::TYPE) == ffi::FALSE {
                return None;
            }
            let reader = create(self.controller, object, T::TYPE, std::ptr::null());
            if reader.is_null() {
                return None;
            }
            let n = count(self.controller, reader).clamp(0, 1 << 20);
            let mut out = Vec::with_capacity(n as usize);
            for i in 0..n {
                let data = event(self.controller, reader, i);
                if !data.is_null() {
                    out.push(T::from_raw(data));
                }
            }
            destroy(self.controller, reader);
            Some(out)
        }
    }

    /// A playback region's content (song time), e.g. its notes.
    pub fn region_content<T: PlugInContent>(&self, region: &str) -> Option<Vec<T>> {
        let r = self.mirror.regions.get(region)?.r;
        self.read(
            self.api.isPlaybackRegionContentAvailable,
            self.api.createPlaybackRegionContentReader,
            r,
        )
    }

    /// An audio modification's content (modification time).
    pub fn modification_content<T: PlugInContent>(&self, modification: &str) -> Option<Vec<T>> {
        let e = self.mirror.modifications.get(modification)?;
        if !e.active {
            return None;
        }
        self.read(
            self.api.isAudioModificationContentAvailable,
            self.api.createAudioModificationContentReader,
            e.r,
        )
    }

    /// An audio source's content as the plug-in analysed it (source time).
    pub fn source_content<T: PlugInContent>(&self, source: &str) -> Option<Vec<T>> {
        let e = self.mirror.sources.get(source)?;
        if !e.active {
            return None;
        }
        self.read(
            self.api.isAudioSourceContentAvailable,
            self.api.createAudioSourceContentReader,
            e.r,
        )
    }
}

impl Drop for Document {
    fn drop(&mut self) {
        // Every object first (ARA's rule), then the controller.
        self.begin();
        let m = std::mem::take(&mut self.mirror);
        for e in m.regions.values() {
            if let Some(f) = self.api.destroyPlaybackRegion {
                // SAFETY: inside an edit cycle; renderers bound to this
                // document are gone or never need the region again.
                unsafe { f(self.controller, e.r) };
            }
        }
        for e in m.modifications.values() {
            if let Some(f) = self.api.destroyAudioModification {
                // SAFETY: inside an edit cycle; its regions are gone.
                unsafe { f(self.controller, e.r) };
            }
        }
        for e in m.sources.values() {
            if let Some(f) = self.api.destroyAudioSource {
                // SAFETY: inside an edit cycle; its modifications are gone.
                unsafe { f(self.controller, e.r) };
            }
        }
        for e in m.sequences.values() {
            if let Some(f) = self.api.destroyRegionSequence {
                // SAFETY: inside an edit cycle; no region refers to it.
                unsafe { f(self.controller, e.r) };
            }
        }
        if let (Some(r), Some(f)) = (m.context, self.api.destroyMusicalContext) {
            // SAFETY: inside an edit cycle; no sequence refers to it.
            unsafe { f(self.controller, r) };
        }
        self.end();
        if let Some(f) = self.api.destroyDocumentController {
            // SAFETY: every object of the graph is gone; called once.
            unsafe { f(self.controller) };
        }
        // The host boxes (`m`'s entries, the core) go only now: the plug-in
        // may refer to them until its controller is destroyed.
        drop(m);
    }
}
