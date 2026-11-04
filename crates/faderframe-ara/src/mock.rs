//! A plug-in side of ARA written in Rust (feature `mock`): a factory, a
//! document controller, a playback renderer, an editor renderer and an
//! editor view, for testing the host without a real plug-in.
//!
//! It behaves like a small Melodyne: at the end of an edit cycle it reads
//! every new source through the host's audio readers ("analysis"), finds
//! notes in it (runs of sound, pitch from zero crossings), keeps an edit
//! per audio modification (a gain the test sets, as a user would in the
//! plug-in's editor), stores and restores those edits in archives, reads
//! the host's tempo map, renders its playback regions at the song position
//! it is given (ignoring its input), and reports what changed when the
//! host asks for model updates.
//!
//! It also checks the host keeps ARA's rules — edits only inside edit
//! cycles, regions out of renderers before they are destroyed, playback
//! renderers changed only while inactive, objects deactivated and
//! destroyed in order, a document emptied before its controller goes —
//! and lists every breach ([`Instance::violations`]).

use crate::content::{Note, TempoEntry};
use crate::{ControllerRef, Factory, Roles, ffi};
use std::collections::BTreeMap;
use std::ffi::{CStr, c_char, c_void};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

pub const FACTORY_ID: &str = "org.faderframe.ara-mock.factory.1";
pub const ARCHIVE_ID: &str = "org.faderframe.ara-mock.archive.2";
/// An older archive format the mock still reads.
pub const OLD_ARCHIVE_ID: &str = "org.faderframe.ara-mock.archive.1";

struct SyncFactory(ffi::Factory);
// SAFETY: every pointer in it is to static, immutable data.
unsafe impl Sync for SyncFactory {}

struct SyncIds([*const c_char; 1]);
// SAFETY: pointers to static C string literals.
unsafe impl Sync for SyncIds {}

static COMPATIBLE: SyncIds = SyncIds([c"org.faderframe.ara-mock.archive.1".as_ptr()]);
static ANALYZABLE: [ffi::ContentType; 1] = [ffi::kARAContentTypeNotes];

static FACTORY: SyncFactory = SyncFactory(ffi::Factory {
    structSize: std::mem::size_of::<ffi::Factory>(),
    lowestSupportedApiGeneration: ffi::kARAAPIGeneration_2_0_Final,
    highestSupportedApiGeneration: ffi::kARAAPIGeneration_2_3_Final,
    factoryID: c"org.faderframe.ara-mock.factory.1".as_ptr(),
    initializeARAWithConfiguration: Some(initialize),
    uninitializeARA: Some(uninitialize),
    plugInName: c"ARA Mock".as_ptr(),
    manufacturerName: c"FaderFrame".as_ptr(),
    informationURL: c"https://github.com/BurningTreeC/faderframe".as_ptr(),
    version: c"1.0.0".as_ptr(),
    createDocumentControllerWithDocument: Some(create_document_controller),
    documentArchiveID: c"org.faderframe.ara-mock.archive.2".as_ptr(),
    compatibleDocumentArchiveIDsCount: 1,
    compatibleDocumentArchiveIDs: &COMPATIBLE.0 as *const [*const c_char; 1]
        as *const *const c_char,
    analyzeableContentTypesCount: 1,
    analyzeableContentTypes: &ANALYZABLE as *const [ffi::ContentType; 1] as *const ffi::ContentType,
    supportedPlaybackTransformationFlags: ffi::kARAPlaybackTransformationTimestretch,
    supportsStoringAudioFileChunks: ffi::FALSE,
});

static INITIALISED: AtomicBool = AtomicBool::new(false);

/// The mock's ARA factory.
pub fn factory() -> Factory {
    // SAFETY: a static factory valid for the life of the process.
    unsafe { Factory::from_raw(&FACTORY.0 as *const ffi::Factory as *const c_void) }
        .unwrap_or_else(|| unreachable!("the mock factory is complete"))
}

/// The factory's address (what a companion API hands out).
pub fn factory_ptr() -> *const c_void {
    &FACTORY.0 as *const ffi::Factory as *const c_void
}

/// Whether ARA was initialised for the mock in this process.
pub fn initialised() -> bool {
    INITIALISED.load(Ordering::Relaxed)
}

unsafe extern "C" fn initialize(config: *const ffi::InterfaceConfiguration) {
    if !config.is_null() {
        // SAFETY: the host's configuration, valid for the call.
        let generation = unsafe { (*config).desiredApiGeneration };
        assert!(generation >= ffi::kARAAPIGeneration_2_0_Final);
    }
    INITIALISED.store(true, Ordering::Relaxed);
}

unsafe extern "C" fn uninitialize() {}

// --- the document ----------------------------------------------------------

#[derive(Clone)]
struct MockSource {
    /// The host's reference (kept as an address: the state is `Send`).
    host: usize,
    persistent: String,
    frames: i64,
    rate: f64,
    channels: usize,
    access: bool,
    active: bool,
    samples: Option<Arc<Vec<Vec<f32>>>>,
    notes: Vec<Note>,
    /// Content changed since the last model update (analysis done).
    changed: bool,
    reported_start: bool,
}

#[derive(Clone)]
struct MockModification {
    host: usize,
    source: usize,
    persistent: String,
    gain: f32,
    active: bool,
    changed: bool,
}

#[derive(Clone, Copy)]
struct MockRegion {
    modification: usize,
    sequence: usize,
    flags: i32,
    start_mod: f64,
    dur_mod: f64,
    start_play: f64,
    dur_play: f64,
}

#[derive(Default)]
struct DocState {
    editing: bool,
    violations: Vec<String>,
    context: Option<usize>,
    /// The host's reference for the context (content readers name it).
    context_host: usize,
    tempo: Vec<TempoEntry>,
    sequences: BTreeMap<usize, String>,
    sources: BTreeMap<usize, MockSource>,
    modifications: BTreeMap<usize, MockModification>,
    regions: BTreeMap<usize, MockRegion>,
    /// Renderers holding each region.
    assigned: BTreeMap<usize, usize>,
    next: usize,
    restored: usize,
    stored: usize,
    document_data: String,
    document_changed: bool,
    destroyed: bool,
    /// The document's name (the host's project).
    name: String,
}

impl DocState {
    fn id(&mut self) -> usize {
        self.next += 1;
        self.next
    }

    fn violation(&mut self, what: impl Into<String>) {
        let what = what.into();
        tracing::warn!("ARA mock: {what}");
        self.violations.push(what);
    }

    fn editing(&mut self, call: &str) {
        if !self.editing {
            self.violation(format!("{call} outside an edit cycle"));
        }
    }
}

struct MockDoc {
    host: ffi::DocumentControllerHostInstance,
    instance: ffi::DocumentControllerInstance,
    state: Arc<Mutex<DocState>>,
}

fn id_ref(id: usize) -> *mut c_void {
    id as *mut c_void
}

fn ref_id(r: *mut c_void) -> usize {
    r as usize
}

/// # Safety
/// `dc` is a controller this mock made and has not destroyed.
unsafe fn doc<'a>(dc: ffi::DocumentControllerRef) -> &'a MockDoc {
    // SAFETY: the caller's contract.
    unsafe { &*(dc as *const MockDoc) }
}

fn with<R>(dc: ffi::DocumentControllerRef, f: impl FnOnce(&mut DocState) -> R) -> R {
    // SAFETY: the host passes back the controller this mock made.
    let d = unsafe { doc(dc) };
    let mut s = d.state.lock().unwrap_or_else(|e| e.into_inner());
    f(&mut s)
}

/// # Safety
/// `s` is null or a valid C string.
unsafe fn string(s: *const c_char) -> String {
    if s.is_null() {
        String::new()
    } else {
        // SAFETY: the caller's contract.
        unsafe { CStr::from_ptr(s) }.to_string_lossy().into_owned()
    }
}

static INTERFACE: ffi::DocumentControllerInterface = ffi::DocumentControllerInterface {
    structSize: std::mem::size_of::<ffi::DocumentControllerInterface>(),
    destroyDocumentController: Some(destroy_controller),
    getFactory: Some(get_factory),
    beginEditing: Some(begin_editing),
    endEditing: Some(end_editing),
    notifyModelUpdates: Some(notify_model_updates),
    beginRestoringDocumentFromArchive: None,
    endRestoringDocumentFromArchive: None,
    storeDocumentToArchive: None,
    updateDocumentProperties: Some(update_document),
    createMusicalContext: Some(create_context),
    updateMusicalContextProperties: Some(update_context_properties),
    updateMusicalContextContent: Some(update_context_content),
    destroyMusicalContext: Some(destroy_context),
    createAudioSource: Some(create_source),
    updateAudioSourceProperties: Some(update_source),
    updateAudioSourceContent: Some(update_source_content),
    enableAudioSourceSamplesAccess: Some(enable_access),
    deactivateAudioSourceForUndoHistory: Some(deactivate_source),
    destroyAudioSource: Some(destroy_source),
    createAudioModification: Some(create_modification),
    cloneAudioModification: None,
    updateAudioModificationProperties: Some(update_modification),
    deactivateAudioModificationForUndoHistory: Some(deactivate_modification),
    destroyAudioModification: Some(destroy_modification),
    createPlaybackRegion: Some(create_region),
    updatePlaybackRegionProperties: Some(update_region),
    destroyPlaybackRegion: Some(destroy_region),
    isAudioSourceContentAvailable: Some(source_available),
    isAudioSourceContentAnalysisIncomplete: Some(source_incomplete),
    requestAudioSourceContentAnalysis: Some(request_analysis),
    getAudioSourceContentGrade: Some(source_grade),
    createAudioSourceContentReader: Some(source_reader),
    isAudioModificationContentAvailable: Some(no_content),
    getAudioModificationContentGrade: Some(no_grade),
    createAudioModificationContentReader: Some(no_reader),
    isPlaybackRegionContentAvailable: Some(region_available),
    getPlaybackRegionContentGrade: Some(no_grade),
    createPlaybackRegionContentReader: Some(region_reader),
    getContentReaderEventCount: Some(reader_count),
    getContentReaderDataForEvent: Some(reader_event),
    destroyContentReader: Some(reader_destroy),
    createRegionSequence: Some(create_sequence),
    updateRegionSequenceProperties: Some(update_sequence),
    destroyRegionSequence: Some(destroy_sequence),
    getPlaybackRegionHeadAndTailTime: Some(head_and_tail),
    restoreObjectsFromArchive: Some(restore),
    storeObjectsToArchive: Some(store),
    getProcessingAlgorithmsCount: None,
    getProcessingAlgorithmProperties: None,
    getProcessingAlgorithmForAudioSource: None,
    requestProcessingAlgorithmForAudioSource: None,
    isLicensedForCapabilities: None,
    storeAudioSourceToAudioFileChunk: None,
    isAudioModificationPreservingAudioSourceSignal: None,
};

unsafe extern "C" fn create_document_controller(
    host: *const ffi::DocumentControllerHostInstance,
    props: *const ffi::DocumentProperties,
) -> *const ffi::DocumentControllerInstance {
    if host.is_null() {
        return std::ptr::null();
    }
    // SAFETY: the host's properties, valid for the call.
    let name = if props.is_null() {
        String::new()
    } else {
        unsafe { string(props.read_unaligned().name) }
    };
    // SAFETY: the host's instance struct, valid until the controller goes
    // (copied: it holds only references).
    let host = unsafe { host.read_unaligned() };
    let mut d = Box::new(MockDoc {
        host,
        instance: ffi::DocumentControllerInstance {
            structSize: std::mem::size_of::<ffi::DocumentControllerInstance>(),
            documentControllerRef: std::ptr::null_mut(),
            documentControllerInterface: &INTERFACE,
        },
        state: Arc::new(Mutex::new(DocState {
            name,
            ..DocState::default()
        })),
    });
    d.instance.documentControllerRef = &*d as *const MockDoc as *mut c_void;
    let d = Box::into_raw(d);
    // SAFETY: just made; stays until `destroy_controller`.
    unsafe { &(*d).instance }
}

unsafe extern "C" fn destroy_controller(dc: ffi::DocumentControllerRef) {
    // SAFETY: made by `create_document_controller`, destroyed once.
    let d = unsafe { Box::from_raw(dc as *mut MockDoc) };
    let mut s = d.state.lock().unwrap_or_else(|e| e.into_inner());
    if !(s.sources.is_empty()
        && s.modifications.is_empty()
        && s.regions.is_empty()
        && s.sequences.is_empty()
        && s.context.is_none())
    {
        s.violation("document controller destroyed with objects left");
    }
    s.destroyed = true;
}

unsafe extern "C" fn get_factory(_dc: ffi::DocumentControllerRef) -> *const ffi::Factory {
    &FACTORY.0
}

unsafe extern "C" fn begin_editing(dc: ffi::DocumentControllerRef) {
    with(dc, |s| {
        if s.editing {
            s.violation("beginEditing while editing");
        }
        s.editing = true;
    });
}

unsafe extern "C" fn end_editing(dc: ffi::DocumentControllerRef) {
    let (work, context) = with(dc, |s| {
        if !s.editing {
            s.violation("endEditing without beginEditing");
        }
        s.editing = false;
        let work: Vec<(usize, MockSource)> = s
            .sources
            .iter()
            .filter(|(_, x)| x.access && x.active && x.samples.is_none())
            .map(|(k, x)| (*k, x.clone()))
            .collect();
        (work, s.context.map(|_| s.context_host))
    });
    // SAFETY: a live controller.
    let d = unsafe { doc(dc) };
    // The musical context's tempo map, through the host's content reader.
    let tempo = context.and_then(|c| read_tempo(&d.host, c as *mut c_void));
    let analysed: Vec<(usize, Vec<Vec<f32>>)> = work
        .into_iter()
        .map(|(id, src)| (id, read_source(&d.host, &src)))
        .collect();
    with(dc, |s| {
        if let Some(t) = tempo {
            s.tempo = t;
        }
        for (id, samples) in analysed {
            if let Some(src) = s.sources.get_mut(&id) {
                src.notes = find_notes(&samples, src.rate);
                src.samples = Some(Arc::new(samples));
                src.changed = true;
            }
        }
    });
}

/// Read a whole source through an audio reader, in chunks.
fn read_source(host: &ffi::DocumentControllerHostInstance, src: &MockSource) -> Vec<Vec<f32>> {
    let frames = src.frames.max(0) as usize;
    let mut out = vec![vec![0.0f32; frames]; src.channels.max(1)];
    let api = host.audioAccessControllerInterface;
    if api.is_null() {
        return out;
    }
    // SAFETY: the host's interface table, valid while the controller lives.
    let api = unsafe { api.read_unaligned() };
    let ctrl = host.audioAccessControllerHostRef;
    // SAFETY: a live source with access enabled (checked by the caller),
    // read from `endEditing` (allowed by ARA).
    let reader =
        unsafe { (api.createAudioReaderForSource)(ctrl, src.host as *mut c_void, ffi::FALSE) };
    if reader.is_null() {
        return out;
    }
    const CHUNK: usize = 4096;
    let mut at = 0;
    while at < frames {
        let n = (frames - at).min(CHUNK);
        let pointers: Vec<*mut c_void> = out
            .iter_mut()
            .map(|c| c[at..at + n].as_mut_ptr() as *mut c_void)
            .collect();
        // SAFETY: one buffer per channel of `n` floats.
        unsafe { (api.readAudioSamples)(ctrl, reader, at as i64, n as i64, pointers.as_ptr()) };
        at += n;
    }
    // SAFETY: made just above, no read running.
    unsafe { (api.destroyAudioReader)(ctrl, reader) };
    out
}

fn read_tempo(
    host: &ffi::DocumentControllerHostInstance,
    context: ffi::MusicalContextHostRef,
) -> Option<Vec<TempoEntry>> {
    let api = host.contentAccessControllerInterface;
    if api.is_null() {
        return None;
    }
    // SAFETY: the host's interface table.
    let api = unsafe { api.read_unaligned() };
    let ctrl = host.contentAccessControllerHostRef;
    // SAFETY: content calls from `endEditing` (allowed by ARA).
    unsafe {
        if (api.isMusicalContextContentAvailable)(ctrl, context, ffi::kARAContentTypeTempoEntries)
            == ffi::FALSE
        {
            return None;
        }
        let r = (api.createMusicalContextContentReader)(
            ctrl,
            context,
            ffi::kARAContentTypeTempoEntries,
            std::ptr::null(),
        );
        if r.is_null() {
            return None;
        }
        let n = (api.getContentReaderEventCount)(ctrl, r);
        let mut out = Vec::new();
        for i in 0..n {
            let e = (api.getContentReaderDataForEvent)(ctrl, r, i) as *const ffi::ContentTempoEntry;
            if !e.is_null() {
                let e = e.read_unaligned();
                out.push(TempoEntry {
                    seconds: e.timePosition,
                    quarters: e.quarterPosition,
                });
            }
        }
        (api.destroyContentReader)(ctrl, r);
        Some(out)
    }
}

/// Runs of sound (gaps under 20 ms joined), pitch from zero crossings.
fn find_notes(samples: &[Vec<f32>], rate: f64) -> Vec<Note> {
    let Some(x) = samples.first() else {
        return Vec::new();
    };
    let gap = (rate * 0.02) as usize;
    let mut notes = Vec::new();
    let mut i = 0;
    while i < x.len() {
        if x[i].abs() < 1e-3 {
            i += 1;
            continue;
        }
        let start = i;
        let mut last = i;
        while i < x.len() && i - last <= gap {
            if x[i].abs() >= 1e-3 {
                last = i;
            }
            i += 1;
        }
        let run = &x[start..=last];
        let crossings = run
            .windows(2)
            .filter(|w| (w[0] < 0.0) != (w[1] < 0.0))
            .count();
        let seconds = run.len() as f64 / rate;
        let frequency = crossings as f64 / (2.0 * seconds);
        let pitch =
            (frequency > 20.0).then(|| (69.0 + 12.0 * (frequency / 440.0).log2()).round() as i32);
        let peak = run.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        notes.push(Note {
            frequency: frequency as f32,
            pitch,
            volume: peak.min(1.0),
            start: start as f64 / rate,
            attack: 0.0,
            length: seconds,
            signal: seconds,
        });
    }
    notes
}

unsafe extern "C" fn notify_model_updates(dc: ffi::DocumentControllerRef) {
    let (sources, modifications, document, started) = with(dc, |s| {
        if s.editing {
            s.violation("notifyModelUpdates while editing");
        }
        let mut started = Vec::new();
        let mut sources = Vec::new();
        for x in s.sources.values_mut() {
            if x.access && x.active && !x.reported_start {
                x.reported_start = true;
                started.push(x.host);
            }
            if x.changed {
                x.changed = false;
                sources.push(x.host);
            }
        }
        let modifications: Vec<_> = s
            .modifications
            .values_mut()
            .filter_map(|m| std::mem::take(&mut m.changed).then_some(m.host))
            .collect();
        let document = std::mem::take(&mut s.document_changed);
        (sources, modifications, document, started)
    });
    // SAFETY: a live controller.
    let d = unsafe { doc(dc) };
    let api = d.host.modelUpdateControllerInterface;
    if api.is_null() {
        return;
    }
    // SAFETY: the host's interface table; called from notifyModelUpdates
    // (the only place ARA allows).
    unsafe {
        let api = api.read_unaligned();
        let ctrl = d.host.modelUpdateControllerHostRef;
        for h in started {
            let h = h as *mut c_void;
            (api.notifyAudioSourceAnalysisProgress)(ctrl, h, ffi::kARAAnalysisProgressStarted, 0.0);
        }
        for h in sources {
            let h = h as *mut c_void;
            (api.notifyAudioSourceAnalysisProgress)(
                ctrl,
                h,
                ffi::kARAAnalysisProgressCompleted,
                1.0,
            );
            (api.notifyAudioSourceContentChanged)(
                ctrl,
                h,
                std::ptr::null(),
                ffi::kARAContentUpdateEverythingChanged,
            );
        }
        for h in modifications {
            let h = h as *mut c_void;
            (api.notifyAudioModificationContentChanged)(
                ctrl,
                h,
                std::ptr::null(),
                ffi::kARAContentUpdateSignalScopeRemainsUnchanged,
            );
        }
        if document {
            (api.notifyDocumentDataChanged)(ctrl);
        }
    }
}

unsafe extern "C" fn update_document(
    dc: ffi::DocumentControllerRef,
    props: *const ffi::DocumentProperties,
) {
    // SAFETY: as above.
    let name = unsafe { string(props.read_unaligned().name) };
    with(dc, |s| {
        s.editing("updateDocumentProperties");
        s.name = name;
    });
}

unsafe extern "C" fn create_context(
    dc: ffi::DocumentControllerRef,
    host: ffi::MusicalContextHostRef,
    _props: *const ffi::MusicalContextProperties,
) -> ffi::MusicalContextRef {
    with(dc, |s| {
        s.editing("createMusicalContext");
        if s.context.is_some() {
            s.violation("a second musical context");
        }
        let id = s.id();
        s.context = Some(id);
        s.context_host = host as usize;
        id_ref(id)
    })
}

unsafe extern "C" fn update_context_properties(
    dc: ffi::DocumentControllerRef,
    _r: ffi::MusicalContextRef,
    _props: *const ffi::MusicalContextProperties,
) {
    with(dc, |s| s.editing("updateMusicalContextProperties"));
}

unsafe extern "C" fn update_context_content(
    dc: ffi::DocumentControllerRef,
    r: ffi::MusicalContextRef,
    _range: *const ffi::ContentTimeRange,
    _flags: ffi::ContentUpdateFlags,
) {
    with(dc, |s| {
        s.editing("updateMusicalContextContent");
        if s.context != Some(ref_id(r)) {
            s.violation("updateMusicalContextContent of an unknown context");
        }
    });
}

unsafe extern "C" fn destroy_context(dc: ffi::DocumentControllerRef, r: ffi::MusicalContextRef) {
    with(dc, |s| {
        s.editing("destroyMusicalContext");
        if !s.sequences.is_empty() {
            s.violation("musical context destroyed while sequences use it");
        }
        if s.context == Some(ref_id(r)) {
            s.context = None;
        }
    });
}

unsafe extern "C" fn create_sequence(
    dc: ffi::DocumentControllerRef,
    _host: ffi::RegionSequenceHostRef,
    props: *const ffi::RegionSequenceProperties,
) -> ffi::RegionSequenceRef {
    // SAFETY: the host's properties, valid for the call.
    let p = unsafe { props.read_unaligned() };
    // SAFETY: as above.
    let name = unsafe { string(p.name) };
    with(dc, |s| {
        s.editing("createRegionSequence");
        if s.context.is_none_or(|c| c != ref_id(p.musicalContextRef)) {
            s.violation("region sequence without its musical context");
        }
        let id = s.id();
        s.sequences.insert(id, name);
        id_ref(id)
    })
}

unsafe extern "C" fn update_sequence(
    dc: ffi::DocumentControllerRef,
    r: ffi::RegionSequenceRef,
    props: *const ffi::RegionSequenceProperties,
) {
    // SAFETY: as above.
    let name = unsafe { string(props.read_unaligned().name) };
    with(dc, |s| {
        s.editing("updateRegionSequenceProperties");
        match s.sequences.get_mut(&ref_id(r)) {
            Some(n) => *n = name,
            None => s.violation("update of an unknown sequence"),
        }
    });
}

unsafe extern "C" fn destroy_sequence(dc: ffi::DocumentControllerRef, r: ffi::RegionSequenceRef) {
    with(dc, |s| {
        s.editing("destroyRegionSequence");
        let id = ref_id(r);
        if s.regions.values().any(|x| x.sequence == id) {
            s.violation("sequence destroyed while regions use it");
        }
        s.sequences.remove(&id);
    });
}

unsafe extern "C" fn create_source(
    dc: ffi::DocumentControllerRef,
    host: ffi::AudioSourceHostRef,
    props: *const ffi::AudioSourceProperties,
) -> ffi::AudioSourceRef {
    // SAFETY: as above.
    let p = unsafe { props.read_unaligned() };
    // SAFETY: as above.
    let persistent = unsafe { string(p.persistentID) };
    with(dc, |s| {
        s.editing("createAudioSource");
        if persistent.is_empty() || !persistent.is_ascii() {
            s.violation("audio source without a 7-bit persistent ID");
        }
        if s.sources.values().any(|x| x.persistent == persistent) {
            s.violation(format!("two audio sources with persistent ID {persistent}"));
        }
        let id = s.id();
        s.sources.insert(
            id,
            MockSource {
                host: host as usize,
                persistent,
                frames: p.sampleCount,
                rate: p.sampleRate,
                channels: p.channelCount.max(1) as usize,
                access: false,
                active: true,
                samples: None,
                notes: Vec::new(),
                changed: false,
                reported_start: false,
            },
        );
        id_ref(id)
    })
}

unsafe extern "C" fn update_source(
    dc: ffi::DocumentControllerRef,
    r: ffi::AudioSourceRef,
    props: *const ffi::AudioSourceProperties,
) {
    // SAFETY: as above.
    let p = unsafe { props.read_unaligned() };
    with(dc, |s| {
        s.editing("updateAudioSourceProperties");
        let Some(src) = s.sources.get(&ref_id(r)).cloned() else {
            s.violation("update of an unknown source");
            return;
        };
        let reshaped = src.frames != p.sampleCount
            || src.rate != p.sampleRate
            || src.channels != p.channelCount.max(1) as usize;
        if reshaped && src.access {
            s.violation("source reshaped while its samples are accessible");
        }
        if let Some(x) = s.sources.get_mut(&ref_id(r)) {
            x.frames = p.sampleCount;
            x.rate = p.sampleRate;
            x.channels = p.channelCount.max(1) as usize;
            if reshaped {
                x.samples = None;
            }
        }
    });
}

unsafe extern "C" fn update_source_content(
    dc: ffi::DocumentControllerRef,
    r: ffi::AudioSourceRef,
    _range: *const ffi::ContentTimeRange,
    _flags: ffi::ContentUpdateFlags,
) {
    with(dc, |s| {
        s.editing("updateAudioSourceContent");
        if let Some(x) = s.sources.get_mut(&ref_id(r)) {
            x.samples = None;
        }
    });
}

unsafe extern "C" fn enable_access(
    dc: ffi::DocumentControllerRef,
    r: ffi::AudioSourceRef,
    on: ffi::Bool,
) {
    with(dc, |s| match s.sources.get_mut(&ref_id(r)) {
        Some(x) => x.access = on != ffi::FALSE,
        None => s.violation("enableAudioSourceSamplesAccess of an unknown source"),
    });
}

unsafe extern "C" fn deactivate_source(
    dc: ffi::DocumentControllerRef,
    r: ffi::AudioSourceRef,
    deactivate: ffi::Bool,
) {
    with(dc, |s| {
        s.editing("deactivateAudioSourceForUndoHistory");
        let id = ref_id(r);
        let off = deactivate != ffi::FALSE;
        if off && s.modifications.values().any(|m| m.source == id && m.active) {
            s.violation("source deactivated before its modifications");
        }
        match s.sources.get_mut(&id) {
            Some(x) => x.active = !off,
            None => s.violation("deactivation of an unknown source"),
        }
    });
}

unsafe extern "C" fn destroy_source(dc: ffi::DocumentControllerRef, r: ffi::AudioSourceRef) {
    with(dc, |s| {
        s.editing("destroyAudioSource");
        let id = ref_id(r);
        if s.modifications.values().any(|m| m.source == id) {
            s.violation("source destroyed before its modifications");
        }
        s.sources.remove(&id);
    });
}

unsafe extern "C" fn create_modification(
    dc: ffi::DocumentControllerRef,
    source: ffi::AudioSourceRef,
    host: ffi::AudioModificationHostRef,
    props: *const ffi::AudioModificationProperties,
) -> ffi::AudioModificationRef {
    // SAFETY: as above.
    let persistent = unsafe { string(props.read_unaligned().persistentID) };
    with(dc, |s| {
        s.editing("createAudioModification");
        let src = ref_id(source);
        if !s.sources.get(&src).is_some_and(|x| x.active) {
            s.violation("modification of an unknown or inactive source");
        }
        if s.modifications.values().any(|m| m.persistent == persistent) {
            s.violation(format!("two modifications with persistent ID {persistent}"));
        }
        let id = s.id();
        s.modifications.insert(
            id,
            MockModification {
                host: host as usize,
                source: src,
                persistent,
                gain: 1.0,
                active: true,
                changed: false,
            },
        );
        id_ref(id)
    })
}

unsafe extern "C" fn update_modification(
    dc: ffi::DocumentControllerRef,
    r: ffi::AudioModificationRef,
    _props: *const ffi::AudioModificationProperties,
) {
    with(dc, |s| {
        s.editing("updateAudioModificationProperties");
        if !s.modifications.contains_key(&ref_id(r)) {
            s.violation("update of an unknown modification");
        }
    });
}

unsafe extern "C" fn deactivate_modification(
    dc: ffi::DocumentControllerRef,
    r: ffi::AudioModificationRef,
    deactivate: ffi::Bool,
) {
    with(dc, |s| {
        s.editing("deactivateAudioModificationForUndoHistory");
        let id = ref_id(r);
        let off = deactivate != ffi::FALSE;
        if off && s.regions.values().any(|x| x.modification == id) {
            s.violation("modification deactivated before its regions were destroyed");
        }
        let source = s.modifications.get(&id).map(|m| m.source);
        if !off && source.is_some_and(|src| !s.sources.get(&src).is_some_and(|x| x.active)) {
            s.violation("modification re-activated before its source");
        }
        match s.modifications.get_mut(&id) {
            Some(m) => m.active = !off,
            None => s.violation("deactivation of an unknown modification"),
        }
    });
}

unsafe extern "C" fn destroy_modification(
    dc: ffi::DocumentControllerRef,
    r: ffi::AudioModificationRef,
) {
    with(dc, |s| {
        s.editing("destroyAudioModification");
        let id = ref_id(r);
        if s.regions.values().any(|x| x.modification == id) {
            s.violation("modification destroyed before its regions");
        }
        s.modifications.remove(&id);
    });
}

fn region_of(p: &ffi::PlaybackRegionProperties) -> (i32, f64, f64, f64, f64, usize) {
    (
        p.transformationFlags,
        p.startInModificationTime,
        p.durationInModificationTime,
        p.startInPlaybackTime,
        p.durationInPlaybackTime,
        ref_id(p.regionSequenceRef),
    )
}

unsafe extern "C" fn create_region(
    dc: ffi::DocumentControllerRef,
    modification: ffi::AudioModificationRef,
    _host: ffi::PlaybackRegionHostRef,
    props: *const ffi::PlaybackRegionProperties,
) -> ffi::PlaybackRegionRef {
    // SAFETY: as above.
    let p = unsafe { props.read_unaligned() };
    let (flags, start_mod, dur_mod, start_play, dur_play, sequence) = region_of(&p);
    with(dc, |s| {
        s.editing("createPlaybackRegion");
        let m = ref_id(modification);
        if !s.modifications.get(&m).is_some_and(|x| x.active) {
            s.violation("region of an unknown or inactive modification");
        }
        if !s.sequences.contains_key(&sequence) {
            s.violation("region on an unknown sequence");
        }
        if flags & ffi::kARAPlaybackTransformationTimestretch == 0 && dur_mod != dur_play {
            s.violation("unstretched region with differing durations");
        }
        let id = s.id();
        s.regions.insert(
            id,
            MockRegion {
                modification: m,
                sequence,
                flags,
                start_mod,
                dur_mod,
                start_play,
                dur_play,
            },
        );
        id_ref(id)
    })
}

unsafe extern "C" fn update_region(
    dc: ffi::DocumentControllerRef,
    r: ffi::PlaybackRegionRef,
    props: *const ffi::PlaybackRegionProperties,
) {
    // SAFETY: as above.
    let p = unsafe { props.read_unaligned() };
    let (flags, start_mod, dur_mod, start_play, dur_play, sequence) = region_of(&p);
    with(dc, |s| {
        s.editing("updatePlaybackRegionProperties");
        if !s.sequences.contains_key(&sequence) {
            s.violation("region moved to an unknown sequence");
        }
        match s.regions.get_mut(&ref_id(r)) {
            Some(x) => {
                x.flags = flags;
                x.start_mod = start_mod;
                x.dur_mod = dur_mod;
                x.start_play = start_play;
                x.dur_play = dur_play;
                x.sequence = sequence;
            }
            None => s.violation("update of an unknown region"),
        }
    });
}

unsafe extern "C" fn destroy_region(dc: ffi::DocumentControllerRef, r: ffi::PlaybackRegionRef) {
    with(dc, |s| {
        s.editing("destroyPlaybackRegion");
        let id = ref_id(r);
        if s.assigned.get(&id).copied().unwrap_or(0) > 0 {
            s.violation("region destroyed while a renderer holds it");
        }
        s.regions.remove(&id);
    });
}

// --- content ---------------------------------------------------------------

unsafe extern "C" fn source_available(
    dc: ffi::DocumentControllerRef,
    r: ffi::AudioSourceRef,
    kind: ffi::ContentType,
) -> ffi::Bool {
    let ok = with(dc, |s| {
        kind == ffi::kARAContentTypeNotes
            && s.sources
                .get(&ref_id(r))
                .is_some_and(|x| x.samples.is_some())
    });
    if ok { ffi::TRUE } else { ffi::FALSE }
}

unsafe extern "C" fn source_incomplete(
    dc: ffi::DocumentControllerRef,
    r: ffi::AudioSourceRef,
    _kind: ffi::ContentType,
) -> ffi::Bool {
    let done = with(dc, |s| {
        s.sources
            .get(&ref_id(r))
            .is_some_and(|x| x.samples.is_some())
    });
    if done { ffi::FALSE } else { ffi::TRUE }
}

unsafe extern "C" fn request_analysis(
    dc: ffi::DocumentControllerRef,
    _r: ffi::AudioSourceRef,
    count: ffi::Size,
    types: *const ffi::ContentType,
) {
    if count == 0 || types.is_null() {
        with(dc, |s| s.violation("an empty analysis request"));
    }
}

unsafe extern "C" fn source_grade(
    _dc: ffi::DocumentControllerRef,
    _r: ffi::AudioSourceRef,
    _kind: ffi::ContentType,
) -> ffi::ContentGrade {
    ffi::kARAContentGradeDetected
}

unsafe extern "C" fn no_content(
    _dc: ffi::DocumentControllerRef,
    _r: *mut c_void,
    _k: ffi::ContentType,
) -> ffi::Bool {
    ffi::FALSE
}

unsafe extern "C" fn no_grade(
    _dc: ffi::DocumentControllerRef,
    _r: *mut c_void,
    _k: ffi::ContentType,
) -> ffi::ContentGrade {
    ffi::kARAContentGradeInitial
}

unsafe extern "C" fn no_reader(
    _dc: ffi::DocumentControllerRef,
    _r: *mut c_void,
    _k: ffi::ContentType,
    _range: *const ffi::ContentTimeRange,
) -> ffi::ContentReaderRef {
    std::ptr::null_mut()
}

fn content_note(n: &Note) -> ffi::ContentNote {
    ffi::ContentNote {
        frequency: n.frequency,
        pitchNumber: n.pitch.unwrap_or(ffi::kARAInvalidPitchNumber),
        volume: n.volume,
        startPosition: n.start,
        attackDuration: n.attack,
        noteDuration: n.length,
        signalDuration: n.signal,
    }
}

unsafe extern "C" fn source_reader(
    dc: ffi::DocumentControllerRef,
    r: ffi::AudioSourceRef,
    kind: ffi::ContentType,
    _range: *const ffi::ContentTimeRange,
) -> ffi::ContentReaderRef {
    if kind != ffi::kARAContentTypeNotes {
        return std::ptr::null_mut();
    }
    let notes: Vec<ffi::ContentNote> = with(dc, |s| {
        s.sources
            .get(&ref_id(r))
            .map(|x| x.notes.iter().map(content_note).collect())
            .unwrap_or_default()
    });
    Box::into_raw(Box::new(notes)) as *mut c_void
}

/// The notes a region plays, in song time.
fn region_notes(s: &DocState, r: &MockRegion) -> Vec<Note> {
    let Some(m) = s.modifications.get(&r.modification) else {
        return Vec::new();
    };
    let Some(src) = s.sources.get(&m.source) else {
        return Vec::new();
    };
    let ratio = if r.dur_mod > 0.0 {
        r.dur_play / r.dur_mod
    } else {
        1.0
    };
    src.notes
        .iter()
        .filter(|n| n.start < r.start_mod + r.dur_mod && n.start + n.length > r.start_mod)
        .map(|n| Note {
            start: r.start_play + (n.start - r.start_mod) * ratio,
            length: n.length * ratio,
            signal: n.signal * ratio,
            ..*n
        })
        .collect()
}

unsafe extern "C" fn region_available(
    dc: ffi::DocumentControllerRef,
    r: ffi::PlaybackRegionRef,
    kind: ffi::ContentType,
) -> ffi::Bool {
    let ok = with(dc, |s| {
        kind == ffi::kARAContentTypeNotes
            && s.regions.get(&ref_id(r)).is_some_and(|x| {
                s.modifications
                    .get(&x.modification)
                    .and_then(|m| s.sources.get(&m.source))
                    .is_some_and(|src| src.samples.is_some())
            })
    });
    if ok { ffi::TRUE } else { ffi::FALSE }
}

unsafe extern "C" fn region_reader(
    dc: ffi::DocumentControllerRef,
    r: ffi::PlaybackRegionRef,
    kind: ffi::ContentType,
    _range: *const ffi::ContentTimeRange,
) -> ffi::ContentReaderRef {
    if kind != ffi::kARAContentTypeNotes {
        return std::ptr::null_mut();
    }
    let notes: Vec<ffi::ContentNote> = with(dc, |s| {
        s.regions
            .get(&ref_id(r))
            .map(|x| region_notes(s, x).iter().map(content_note).collect())
            .unwrap_or_default()
    });
    Box::into_raw(Box::new(notes)) as *mut c_void
}

unsafe extern "C" fn reader_count(
    _dc: ffi::DocumentControllerRef,
    r: ffi::ContentReaderRef,
) -> ffi::Int32 {
    if r.is_null() {
        return 0;
    }
    // SAFETY: made by a reader function above, not yet destroyed.
    unsafe { (*(r as *const Vec<ffi::ContentNote>)).len() as i32 }
}

unsafe extern "C" fn reader_event(
    _dc: ffi::DocumentControllerRef,
    r: ffi::ContentReaderRef,
    i: ffi::Int32,
) -> *const c_void {
    if r.is_null() || i < 0 {
        return std::ptr::null();
    }
    // SAFETY: as above.
    let v = unsafe { &*(r as *const Vec<ffi::ContentNote>) };
    v.get(i as usize).map_or(std::ptr::null(), |n| {
        n as *const ffi::ContentNote as *const c_void
    })
}

unsafe extern "C" fn reader_destroy(_dc: ffi::DocumentControllerRef, r: ffi::ContentReaderRef) {
    if !r.is_null() {
        // SAFETY: as above, destroyed once.
        drop(unsafe { Box::from_raw(r as *mut Vec<ffi::ContentNote>) });
    }
}

unsafe extern "C" fn head_and_tail(
    _dc: ffi::DocumentControllerRef,
    _r: ffi::PlaybackRegionRef,
    head: *mut ffi::TimeDuration,
    tail: *mut ffi::TimeDuration,
) {
    // SAFETY: the host's out pointers (ARA: never null).
    unsafe {
        *head = 0.0;
        *tail = 0.0;
    }
}

// --- archives --------------------------------------------------------------

unsafe extern "C" fn store(
    dc: ffi::DocumentControllerRef,
    writer: ffi::ArchiveWriterHostRef,
    filter: *const ffi::StoreObjectsFilter,
) -> ffi::Bool {
    let text = with(dc, |s| {
        if s.editing {
            s.violation("storeObjectsToArchive while editing");
        }
        s.stored += 1;
        let (sources, modifications, data) = if filter.is_null() {
            (
                s.sources.keys().copied().collect::<Vec<_>>(),
                s.modifications.keys().copied().collect::<Vec<_>>(),
                true,
            )
        } else {
            // SAFETY: the host's filter, valid for the call.
            let f = unsafe { filter.read_unaligned() };
            let list = |p: *const *mut c_void, n: usize| -> Vec<usize> {
                if p.is_null() {
                    return Vec::new();
                }
                // SAFETY: an array of `n` references.
                (0..n).map(|i| ref_id(unsafe { *p.add(i) })).collect()
            };
            (
                list(f.audioSourceRefs, f.audioSourceRefsCount),
                list(f.audioModificationRefs, f.audioModificationRefsCount),
                f.documentData != ffi::FALSE,
            )
        };
        let mut text = String::new();
        for id in sources {
            if let Some(x) = s.sources.get(&id) {
                text.push_str(&format!("src\t{}\n", x.persistent));
            }
        }
        for id in modifications {
            if let Some(m) = s.modifications.get(&id) {
                text.push_str(&format!("mod\t{}\t{}\n", m.persistent, m.gain));
            }
        }
        if data {
            text.push_str(&format!("doc\t{}\n", s.document_data));
        }
        text
    });
    // SAFETY: a live controller.
    let d = unsafe { doc(dc) };
    let api = d.host.archivingControllerInterface;
    if api.is_null() {
        return ffi::FALSE;
    }
    // SAFETY: the host's interface; the writer is valid for this call.
    unsafe {
        let api = api.read_unaligned();
        let ctrl = d.host.archivingControllerHostRef;
        // In two pieces, the second first (writers must take positions).
        let bytes = text.as_bytes();
        let half = bytes.len() / 2;
        let ok = (api.writeBytesToArchive)(
            ctrl,
            writer,
            half,
            bytes.len() - half,
            bytes[half..].as_ptr(),
        ) != ffi::FALSE
            && (api.writeBytesToArchive)(ctrl, writer, 0, half, bytes.as_ptr()) != ffi::FALSE;
        (api.notifyDocumentArchivingProgress)(ctrl, 1.0);
        if ok { ffi::TRUE } else { ffi::FALSE }
    }
}

unsafe extern "C" fn restore(
    dc: ffi::DocumentControllerRef,
    reader: ffi::ArchiveReaderHostRef,
    _filter: *const ffi::RestoreObjectsFilter,
) -> ffi::Bool {
    // SAFETY: a live controller.
    let d = unsafe { doc(dc) };
    let api = d.host.archivingControllerInterface;
    if api.is_null() {
        return ffi::FALSE;
    }
    // SAFETY: the host's interface; the reader is valid for this call.
    let (bytes, id) = unsafe {
        let api = api.read_unaligned();
        let ctrl = d.host.archivingControllerHostRef;
        let size = (api.getArchiveSize)(ctrl, reader);
        let mut bytes = vec![0u8; size];
        let mut at = 0;
        while at < size {
            let n = (size - at).min(7);
            if (api.readBytesFromArchive)(ctrl, reader, at, n, bytes[at..].as_mut_ptr())
                == ffi::FALSE
            {
                return ffi::FALSE;
            }
            at += n;
        }
        (bytes, string((api.getDocumentArchiveID)(ctrl, reader)))
    };
    with(dc, |s| {
        s.editing("restoreObjectsFromArchive");
        if id != ARCHIVE_ID && id != OLD_ARCHIVE_ID {
            s.violation(format!("archive ID {id} is not the mock's"));
            return ffi::FALSE;
        }
        s.restored += 1;
        for line in String::from_utf8_lossy(&bytes).lines() {
            let parts: Vec<&str> = line.split('\t').collect();
            match parts.as_slice() {
                ["mod", key, gain] => {
                    let gain: f32 = gain.parse().unwrap_or(1.0);
                    if let Some(m) = s.modifications.values_mut().find(|m| m.persistent == *key) {
                        m.gain = gain;
                    }
                }
                ["doc", data] => s.document_data = (*data).to_string(),
                _ => {}
            }
        }
        ffi::TRUE
    })
}

// --- the companion instance ------------------------------------------------

/// What a test reads of the mock's document.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Summary {
    /// Persistent ID, active, analysed.
    pub sources: Vec<(String, bool, bool)>,
    /// Persistent ID, gain, active.
    pub modifications: Vec<(String, f32, bool)>,
    pub regions: usize,
    pub sequences: Vec<String>,
    pub tempo: Vec<TempoEntry>,
    pub restored: usize,
    pub stored: usize,
    pub document_data: String,
    pub editing: bool,
    pub destroyed: bool,
    /// The document's name.
    pub document: String,
}

struct Shared {
    active: AtomicBool,
    bound: AtomicBool,
    roles: Mutex<Roles>,
    regions: Mutex<Vec<usize>>,
    sequences: Mutex<Vec<usize>>,
    selection: Mutex<Vec<usize>>,
    violations: Mutex<Vec<String>>,
    doc: Mutex<Option<(Arc<Mutex<DocState>>, ffi::DocumentControllerHostInstance)>>,
}

// SAFETY: the host instance copy holds references the host keeps valid
// for the document's life and that ARA allows on any thread (playback
// requests); the rest is behind mutexes and atomics.
unsafe impl Send for Shared {}
// SAFETY: as above.
unsafe impl Sync for Shared {}

impl Shared {
    fn violation(&self, what: impl Into<String>) {
        let what = what.into();
        tracing::warn!("ARA mock instance: {what}");
        if let Ok(mut v) = self.violations.lock() {
            v.push(what);
        }
    }

    fn doc(&self) -> Option<Arc<Mutex<DocState>>> {
        self.doc
            .lock()
            .ok()
            .and_then(|d| d.as_ref().map(|(s, _)| Arc::clone(s)))
    }
}

/// The plug-in extension's struct and the instance it points back to.
struct ExtensionBox {
    raw: ffi::PlugInExtensionInstance,
}

/// A companion plug-in instance of the mock (what a VST3 component or a
/// CLAP plug-in would be). Render with [`Instance::render`].
pub struct Instance {
    shared: Arc<Shared>,
    extension: Box<ExtensionBox>,
}

static PLAYBACK: ffi::PlaybackRendererInterface = ffi::PlaybackRendererInterface {
    structSize: std::mem::size_of::<ffi::PlaybackRendererInterface>(),
    addPlaybackRegion: Some(playback_add),
    removePlaybackRegion: Some(playback_remove),
};

static EDITOR: ffi::EditorRendererInterface = ffi::EditorRendererInterface {
    structSize: std::mem::size_of::<ffi::EditorRendererInterface>(),
    addPlaybackRegion: Some(editor_add_region),
    removePlaybackRegion: Some(editor_remove_region),
    addRegionSequence: Some(editor_add_sequence),
    removeRegionSequence: Some(editor_remove_sequence),
};

static VIEW: ffi::EditorViewInterface = ffi::EditorViewInterface {
    structSize: std::mem::size_of::<ffi::EditorViewInterface>(),
    notifySelection: Some(view_selection),
    notifyHideRegionSequences: Some(view_hide),
};

/// # Safety
/// `r` is an instance reference of a live [`Instance`].
unsafe fn shared<'a>(r: *mut c_void) -> &'a Shared {
    // SAFETY: the caller's contract (the Arc's data).
    unsafe { &*(r as *const Shared) }
}

unsafe extern "C" fn playback_add(r: ffi::PlaybackRendererRef, region: ffi::PlaybackRegionRef) {
    // SAFETY: our renderer reference.
    let sh = unsafe { shared(r) };
    if sh.active.load(Ordering::Relaxed) {
        sh.violation("addPlaybackRegion while active");
    }
    let id = ref_id(region);
    if let Some(d) = sh.doc() {
        let mut s = d.lock().unwrap_or_else(|e| e.into_inner());
        if !s.regions.contains_key(&id) {
            s.violation("an unknown region added to a renderer");
        }
        *s.assigned.entry(id).or_insert(0) += 1;
    }
    if let Ok(mut v) = sh.regions.lock() {
        if v.contains(&id) {
            sh.violation("a region added twice");
        }
        v.push(id);
    }
}

unsafe extern "C" fn playback_remove(r: ffi::PlaybackRendererRef, region: ffi::PlaybackRegionRef) {
    // SAFETY: as above.
    let sh = unsafe { shared(r) };
    if sh.active.load(Ordering::Relaxed) {
        sh.violation("removePlaybackRegion while active");
    }
    let id = ref_id(region);
    if let Some(d) = sh.doc() {
        let mut s = d.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(n) = s.assigned.get_mut(&id) {
            *n = n.saturating_sub(1);
        }
    }
    if let Ok(mut v) = sh.regions.lock() {
        let before = v.len();
        v.retain(|x| *x != id);
        if v.len() == before {
            sh.violation("a region removed that the renderer did not have");
        }
    }
}

unsafe extern "C" fn editor_add_region(r: ffi::EditorRendererRef, _region: ffi::PlaybackRegionRef) {
    // SAFETY: as above.
    unsafe { shared(r) }.violation("editor renderer given regions (FaderFrame uses sequences)");
}

unsafe extern "C" fn editor_remove_region(
    _r: ffi::EditorRendererRef,
    _region: ffi::PlaybackRegionRef,
) {
}

unsafe extern "C" fn editor_add_sequence(
    r: ffi::EditorRendererRef,
    sequence: ffi::RegionSequenceRef,
) {
    // SAFETY: as above.
    let sh = unsafe { shared(r) };
    if let Ok(mut v) = sh.sequences.lock() {
        v.push(ref_id(sequence));
    }
}

unsafe extern "C" fn editor_remove_sequence(
    r: ffi::EditorRendererRef,
    sequence: ffi::RegionSequenceRef,
) {
    // SAFETY: as above.
    let sh = unsafe { shared(r) };
    if let Ok(mut v) = sh.sequences.lock() {
        v.retain(|x| *x != ref_id(sequence));
    }
}

unsafe extern "C" fn view_selection(r: ffi::EditorViewRef, selection: *const ffi::ViewSelection) {
    // SAFETY: as above; the selection is valid for the call.
    let (sh, sel) = unsafe { (shared(r), selection.read_unaligned()) };
    let regions: Vec<usize> = if sel.playbackRegionRefs.is_null() {
        Vec::new()
    } else {
        (0..sel.playbackRegionRefsCount)
            // SAFETY: an array of `count` region references.
            .map(|i| ref_id(unsafe { *sel.playbackRegionRefs.add(i) }))
            .collect()
    };
    if let Ok(mut v) = sh.selection.lock() {
        *v = regions;
    }
}

unsafe extern "C" fn view_hide(
    _r: ffi::EditorViewRef,
    _n: ffi::Size,
    _seqs: *const ffi::RegionSequenceRef,
) {
}

impl Default for Instance {
    fn default() -> Self {
        Self::new()
    }
}

impl Instance {
    pub fn new() -> Self {
        let shared = Arc::new(Shared {
            active: AtomicBool::new(false),
            bound: AtomicBool::new(false),
            roles: Mutex::new(Roles(0)),
            regions: Mutex::new(Vec::new()),
            sequences: Mutex::new(Vec::new()),
            selection: Mutex::new(Vec::new()),
            violations: Mutex::new(Vec::new()),
            doc: Mutex::new(None),
        });
        let me = Arc::as_ptr(&shared) as *mut c_void;
        let extension = Box::new(ExtensionBox {
            raw: ffi::PlugInExtensionInstance {
                structSize: std::mem::size_of::<ffi::PlugInExtensionInstance>(),
                plugInExtensionRef: std::ptr::null_mut(),
                plugInExtensionInterface: std::ptr::null(),
                playbackRendererRef: me,
                playbackRendererInterface: &PLAYBACK,
                editorRendererRef: me,
                editorRendererInterface: &EDITOR,
                editorViewRef: me,
                editorViewInterface: &VIEW,
            },
        });
        Instance { shared, extension }
    }

    /// The companion API's binding call: once, with the document
    /// controller's reference; returns the plug-in extension (null when
    /// bound already or the controller is not the mock's).
    pub fn bind(
        &mut self,
        controller: ControllerRef,
        known: Roles,
        assigned: Roles,
    ) -> *const c_void {
        if self.shared.bound.swap(true, Ordering::Relaxed) {
            self.shared.violation("bound twice");
            return std::ptr::null();
        }
        if self.shared.active.load(Ordering::Relaxed) {
            self.shared.violation("bound after activation");
        }
        let dc = controller.as_raw();
        if dc.is_null() {
            return std::ptr::null();
        }
        // SAFETY: the host binds to a controller of a live document; for
        // the mock that is a `MockDoc` (the tests only bind mock
        // instances to mock documents).
        let d = unsafe { doc(dc) };
        if let Ok(mut slot) = self.shared.doc.lock() {
            *slot = Some((Arc::clone(&d.state), d.host));
        }
        if let Ok(mut r) = self.shared.roles.lock() {
            *r = assigned;
        }
        let assigned = assigned.bits() & known.bits();
        let e = &mut self.extension.raw;
        if assigned & ffi::kARAPlaybackRendererRole == 0 {
            e.playbackRendererInterface = std::ptr::null();
        }
        if assigned & ffi::kARAEditorRendererRole == 0 {
            e.editorRendererInterface = std::ptr::null();
        }
        if assigned & ffi::kARAEditorViewRole == 0 {
            e.editorViewInterface = std::ptr::null();
        }
        e as *const ffi::PlugInExtensionInstance as *const c_void
    }

    /// [`Self::bind`] with the extension wrapped (for safe wrappers of the
    /// mock; the extension is valid while this instance lives).
    pub fn bind_extension(
        &mut self,
        controller: ControllerRef,
        known: Roles,
        assigned: Roles,
    ) -> Option<crate::Extension> {
        let raw = self.bind(controller, known, assigned);
        // SAFETY: the extension struct is boxed in this instance and lives
        // as long as it; callers drop the extension before the instance
        // (as with any companion API's extension).
        unsafe { crate::Extension::from_raw(raw) }
    }

    /// The companion API's activate/deactivate.
    pub fn set_active(&self, on: bool) {
        self.shared.active.store(on, Ordering::Relaxed);
    }

    pub fn is_active(&self) -> bool {
        self.shared.active.load(Ordering::Relaxed)
    }

    /// A renderer for the audio thread (any thread).
    pub fn renderer(&self) -> Renderer {
        Renderer {
            shared: Arc::clone(&self.shared),
        }
    }

    /// What tests read of this instance and its document, alive after
    /// the instance (and the document) are gone.
    pub fn probe(&self) -> Probe {
        Probe {
            shared: Arc::clone(&self.shared),
        }
    }

    /// Breaches of ARA's rules seen by this instance and its document.
    pub fn violations(&self) -> Vec<String> {
        self.probe().violations()
    }

    pub fn summary(&self) -> Summary {
        self.probe().summary()
    }

    /// The playback regions this instance renders now.
    pub fn region_count(&self) -> usize {
        self.probe().region_count()
    }

    /// An edit in the plug-in's own editor (see [`Probe::edit`]).
    pub fn edit(&self, modification: &str, gain: f32) -> bool {
        self.probe().edit(modification, gain)
    }

    /// See [`Probe::set_document_data`].
    pub fn set_document_data(&self, data: &str) {
        self.probe().set_document_data(data);
    }

    /// See [`Probe::request`].
    pub fn request(&self, request: crate::PlaybackRequest) {
        self.probe().request(request);
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        // A destroyed companion instance lets go of its regions (the host
        // need not remove them first).
        let regions = self
            .shared
            .regions
            .lock()
            .map(|mut v| std::mem::take(&mut *v))
            .unwrap_or_default();
        if let Some(d) = self.shared.doc() {
            let mut s = d.lock().unwrap_or_else(|e| e.into_inner());
            for id in regions {
                if let Some(n) = s.assigned.get_mut(&id) {
                    *n = n.saturating_sub(1);
                }
            }
        }
    }
}

/// Inspection of an [`Instance`] and its document.
#[derive(Clone)]
pub struct Probe {
    shared: Arc<Shared>,
}

impl Probe {
    /// An edit in the plug-in's own editor: the modification with this
    /// persistent ID plays at `gain` (reported at the next model update).
    pub fn edit(&self, modification: &str, gain: f32) -> bool {
        let Some(d) = self.shared.doc() else {
            return false;
        };
        let mut s = d.lock().unwrap_or_else(|e| e.into_inner());
        match s
            .modifications
            .values_mut()
            .find(|m| m.persistent == modification)
        {
            Some(m) => {
                m.gain = gain;
                m.changed = true;
                true
            }
            None => false,
        }
    }

    /// Private document data (a plug-in's own chord track, say).
    pub fn set_document_data(&self, data: &str) {
        if let Some(d) = self.shared.doc() {
            let mut s = d.lock().unwrap_or_else(|e| e.into_inner());
            s.document_data = data.to_string();
            s.document_changed = true;
        }
    }

    /// The user pressed play in the plug-in's editor.
    pub fn request(&self, request: crate::PlaybackRequest) {
        let host = self
            .shared
            .doc
            .lock()
            .ok()
            .and_then(|d| d.as_ref().map(|(_, h)| *h));
        let Some(host) = host else { return };
        let api = host.playbackControllerInterface;
        if api.is_null() {
            return;
        }
        let ctrl = host.playbackControllerHostRef;
        // SAFETY: the host's interface table, valid while its document is.
        unsafe {
            let api = api.read_unaligned();
            match request {
                crate::PlaybackRequest::Start => (api.requestStartPlayback)(ctrl),
                crate::PlaybackRequest::Stop => (api.requestStopPlayback)(ctrl),
                crate::PlaybackRequest::Locate(t) => (api.requestSetPlaybackPosition)(ctrl, t),
                crate::PlaybackRequest::CycleRange { start, duration } => {
                    (api.requestSetCycleRange)(ctrl, start, duration)
                }
                crate::PlaybackRequest::EnableCycle(on) => {
                    (api.requestEnableCycle)(ctrl, if on { ffi::TRUE } else { ffi::FALSE })
                }
            }
        }
    }
    /// Breaches of ARA's rules seen by this instance and its document.
    pub fn violations(&self) -> Vec<String> {
        let mut v = self
            .shared
            .violations
            .lock()
            .map(|v| v.clone())
            .unwrap_or_default();
        if let Some(d) = self.shared.doc() {
            v.extend(
                d.lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .violations
                    .clone(),
            );
        }
        v
    }

    /// The playback regions this instance renders now.
    pub fn region_count(&self) -> usize {
        self.shared.regions.lock().map(|v| v.len()).unwrap_or(0)
    }

    /// The region sequences its editor renderer previews.
    pub fn sequence_count(&self) -> usize {
        self.shared.sequences.lock().map(|v| v.len()).unwrap_or(0)
    }

    /// How many regions the last selection named.
    pub fn selected(&self) -> usize {
        self.shared.selection.lock().map(|v| v.len()).unwrap_or(0)
    }

    pub fn summary(&self) -> Summary {
        let Some(d) = self.shared.doc() else {
            return Summary::default();
        };
        let s = d.lock().unwrap_or_else(|e| e.into_inner());
        Summary {
            sources: s
                .sources
                .values()
                .map(|x| (x.persistent.clone(), x.active, x.samples.is_some()))
                .collect(),
            modifications: s
                .modifications
                .values()
                .map(|m| (m.persistent.clone(), m.gain, m.active))
                .collect(),
            regions: s.regions.len(),
            sequences: s.sequences.values().cloned().collect(),
            tempo: s.tempo.clone(),
            restored: s.restored,
            stored: s.stored,
            document_data: s.document_data.clone(),
            editing: s.editing,
            destroyed: s.destroyed,
            document: s.name.clone(),
        }
    }
}

/// The audio thread's view of an [`Instance`].
#[derive(Clone)]
pub struct Renderer {
    shared: Arc<Shared>,
}

impl Renderer {
    /// Render the instance's playback regions for the block starting at
    /// song sample `position` (silence while stopped: a playback renderer
    /// ignores its input). Takes no locks it would wait for: a block that
    /// finds the model busy is silent.
    pub fn render(&self, position: i64, sample_rate: f64, playing: bool, out: &mut [&mut [f32]]) {
        for (c, ch) in out.iter_mut().enumerate() {
            self.render_channel(position, sample_rate, playing, c, ch);
        }
    }

    /// One channel of [`Self::render`] (allocation-free).
    pub fn render_channel(
        &self,
        position: i64,
        sample_rate: f64,
        playing: bool,
        channel: usize,
        out: &mut [f32],
    ) {
        out.fill(0.0);
        if !playing || !self.shared.active.load(Ordering::Relaxed) {
            return;
        }
        let Ok(regions) = self.shared.regions.try_lock() else {
            return;
        };
        let Ok(doc) = self.shared.doc.try_lock() else {
            return;
        };
        let Some((doc, _)) = doc.as_ref() else {
            return;
        };
        let Ok(s) = doc.try_lock() else {
            return;
        };
        for id in regions.iter() {
            let Some(r) = s.regions.get(id) else { continue };
            let Some(m) = s.modifications.get(&r.modification) else {
                continue;
            };
            let Some(src) = s.sources.get(&m.source) else {
                continue;
            };
            let Some(samples) = &src.samples else {
                continue;
            };
            let Some(data) = samples.get(channel.min(samples.len().saturating_sub(1))) else {
                continue;
            };
            let ratio =
                if r.flags & ffi::kARAPlaybackTransformationTimestretch != 0 && r.dur_play > 0.0 {
                    r.dur_mod / r.dur_play
                } else {
                    1.0
                };
            for (i, o) in out.iter_mut().enumerate() {
                let t = (position + i as i64) as f64 / sample_rate;
                if t < r.start_play || t >= r.start_play + r.dur_play {
                    continue;
                }
                let source_time = r.start_mod + (t - r.start_play) * ratio;
                let k = (source_time * src.rate).round();
                if k >= 0.0 && (k as usize) < data.len() {
                    *o += data[k as usize] * m.gain;
                }
            }
        }
    }
}
