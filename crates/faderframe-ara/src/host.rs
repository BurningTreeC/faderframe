//! The host's side of the API: the five interfaces a document controller
//! calls back into.
//!
//! Host references are pointers to boxes the [`crate::Document`] owns:
//! [`HostCore`] for the controllers (and the one musical context),
//! [`SourceHost`] for audio sources, [`HostKey`] for modifications,
//! regions and sequences; readers, archives and content readers are boxes
//! of their own, alive from their creating call to their destroying call
//! (or for the duration of the call that hands them over).

use crate::content::Context;
use crate::document::{Analysis, AnalysisState, PlaybackRequest, Updates};
use crate::{Samples, ffi};
use std::ffi::{CString, c_char, c_void};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// Programming errors plug-ins reported (see [`crate::assert_count`]).
pub(crate) static ASSERTS: AtomicUsize = AtomicUsize::new(0);

/// The global assert function's address (handed to every factory).
pub(crate) static ASSERT_FUNCTION: Option<ffi::AssertFunction> = Some(on_assert);

/// ARA's assert function: counted always (realtime-safe), described in
/// debug builds.
unsafe extern "C" fn on_assert(
    category: ffi::AssertCategory,
    _argument: *const c_void,
    diagnosis: *const c_char,
) {
    ASSERTS.fetch_add(1, Ordering::Relaxed);
    #[cfg(debug_assertions)]
    {
        // SAFETY: null or a NUL-terminated string valid for the call.
        let text = unsafe { crate::content::text(diagnosis) }.unwrap_or_default();
        tracing::warn!("ARA assert ({category}): {text}");
    }
    #[cfg(not(debug_assertions))]
    let _ = (category, diagnosis);
}

/// A model object's host reference: its key.
pub(crate) struct HostKey {
    pub key: String,
}

/// An audio source's host reference.
pub(crate) struct SourceHost {
    pub key: String,
    pub channels: usize,
    pub samples: Mutex<Arc<dyn Samples>>,
}

/// The controllers' host reference (and the musical context's).
pub(crate) struct HostCore {
    pub instance: ffi::DocumentControllerHostInstance,
    pub context: Mutex<Context>,
    pub updates: Mutex<Updates>,
}

impl HostCore {
    pub fn new() -> Box<HostCore> {
        let null = std::ptr::null_mut();
        let mut core = Box::new(HostCore {
            instance: ffi::DocumentControllerHostInstance {
                structSize: std::mem::size_of::<ffi::DocumentControllerHostInstance>(),
                audioAccessControllerHostRef: null,
                audioAccessControllerInterface: &AUDIO_ACCESS,
                archivingControllerHostRef: null,
                archivingControllerInterface: &ARCHIVING,
                contentAccessControllerHostRef: null,
                contentAccessControllerInterface: &CONTENT_ACCESS,
                modelUpdateControllerHostRef: null,
                modelUpdateControllerInterface: &MODEL_UPDATE,
                playbackControllerHostRef: null,
                playbackControllerInterface: &PLAYBACK,
            },
            context: Mutex::new(Context::default()),
            updates: Mutex::new(Updates::default()),
        });
        let me = &*core as *const HostCore as *mut c_void;
        core.instance.audioAccessControllerHostRef = me;
        core.instance.archivingControllerHostRef = me;
        core.instance.contentAccessControllerHostRef = me;
        core.instance.modelUpdateControllerHostRef = me;
        core.instance.playbackControllerHostRef = me;
        core
    }

    /// The musical context's host reference.
    pub fn context_ref(&self) -> ffi::MusicalContextHostRef {
        self as *const HostCore as *mut c_void
    }

    fn update(&self, f: impl FnOnce(&mut Updates)) {
        if let Ok(mut u) = self.updates.lock() {
            f(&mut u);
        }
    }
}

/// # Safety
/// `r` is a [`HostCore`] host reference of a live document.
unsafe fn core<'a>(r: *mut c_void) -> &'a HostCore {
    // SAFETY: the caller's contract.
    unsafe { &*(r as *const HostCore) }
}

/// # Safety
/// `r` is a [`HostKey`] host reference of a live object.
unsafe fn key_of(r: *mut c_void) -> String {
    if r.is_null() {
        return String::new();
    }
    // SAFETY: the caller's contract.
    unsafe { (*(r as *const HostKey)).key.clone() }
}

// --- audio access ----------------------------------------------------------

/// An audio reader: one thread at a time.
struct Reader {
    samples: Arc<dyn Samples>,
    channels: usize,
    wide: bool,
    scratch: Vec<Vec<f32>>,
}

/// Frames per read into the scratch buffers (64-bit readers).
const SCRATCH_FRAMES: usize = 1 << 15;

static AUDIO_ACCESS: ffi::AudioAccessControllerInterface = ffi::AudioAccessControllerInterface {
    structSize: std::mem::size_of::<ffi::AudioAccessControllerInterface>(),
    createAudioReaderForSource: create_audio_reader,
    readAudioSamples: read_audio_samples,
    destroyAudioReader: destroy_audio_reader,
};

unsafe extern "C" fn create_audio_reader(
    _controller: ffi::AudioAccessControllerHostRef,
    source: ffi::AudioSourceHostRef,
    use_64_bit: ffi::Bool,
) -> ffi::AudioReaderHostRef {
    if source.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: a source host reference of this document (the plug-in may
    // create readers only for live sources).
    let source = unsafe { &*(source as *const SourceHost) };
    let Ok(samples) = source.samples.lock().map(|s| Arc::clone(&s)) else {
        return std::ptr::null_mut();
    };
    let reader = Box::new(Reader {
        samples,
        channels: source.channels.max(1),
        wide: use_64_bit != ffi::FALSE,
        scratch: Vec::new(),
    });
    Box::into_raw(reader) as *mut c_void
}

unsafe extern "C" fn read_audio_samples(
    _controller: ffi::AudioAccessControllerHostRef,
    reader: ffi::AudioReaderHostRef,
    position: ffi::SamplePosition,
    frames: ffi::SampleCount,
    buffers: *const *mut c_void,
) -> ffi::Bool {
    if reader.is_null() || buffers.is_null() {
        return ffi::FALSE;
    }
    // SAFETY: a reader this host made; ARA uses a reader on one thread at
    // a time, so the exclusive borrow holds.
    let reader = unsafe { &mut *(reader as *mut Reader) };
    let Ok(frames) = usize::try_from(frames) else {
        return ffi::FALSE;
    };
    if frames == 0 {
        return ffi::TRUE;
    }
    let channels = reader.channels;
    // SAFETY: one buffer per channel of the source, each `frames` long, of
    // the sample type the reader was made for (the caller's buffers).
    let pointers = unsafe { std::slice::from_raw_parts(buffers, channels) };
    if !reader.wide {
        let mut out: Vec<&mut [f32]> = pointers
            .iter()
            // SAFETY: as above.
            .map(|&p| unsafe { std::slice::from_raw_parts_mut(p as *mut f32, frames) })
            .collect();
        return if reader.samples.read(position, &mut out) {
            ffi::TRUE
        } else {
            out.iter_mut().for_each(|c| c.fill(0.0));
            ffi::FALSE
        };
    }
    let chunk = frames.min(SCRATCH_FRAMES);
    reader.scratch.resize_with(channels, Vec::new);
    for c in &mut reader.scratch {
        c.resize(chunk, 0.0);
    }
    let mut ok = true;
    let mut done = 0;
    while done < frames {
        let n = (frames - done).min(chunk);
        let mut out: Vec<&mut [f32]> = reader.scratch.iter_mut().map(|c| &mut c[..n]).collect();
        let read = reader.samples.read(position + done as i64, &mut out);
        ok &= read;
        for (c, &p) in pointers.iter().enumerate() {
            // SAFETY: as above, 64-bit buffers.
            let dst = unsafe { std::slice::from_raw_parts_mut(p as *mut f64, frames) };
            for (d, s) in dst[done..done + n].iter_mut().zip(&reader.scratch[c][..n]) {
                *d = if read { f64::from(*s) } else { 0.0 };
            }
        }
        done += n;
    }
    if ok { ffi::TRUE } else { ffi::FALSE }
}

unsafe extern "C" fn destroy_audio_reader(
    _controller: ffi::AudioAccessControllerHostRef,
    reader: ffi::AudioReaderHostRef,
) {
    if !reader.is_null() {
        // SAFETY: made by `create_audio_reader`, destroyed once (the
        // plug-in guarantees no read is running).
        drop(unsafe { Box::from_raw(reader as *mut Reader) });
    }
}

// --- archiving -------------------------------------------------------------

/// An archive being restored (alive for the restoring call).
pub(crate) struct ArchiveIn {
    pub data: Vec<u8>,
    pub id: CString,
}

/// An archive being written (alive for the storing call).
pub(crate) struct ArchiveOut {
    pub data: Mutex<Vec<u8>>,
}

static ARCHIVING: ffi::ArchivingControllerInterface = ffi::ArchivingControllerInterface {
    structSize: std::mem::size_of::<ffi::ArchivingControllerInterface>(),
    getArchiveSize: archive_size,
    readBytesFromArchive: read_archive,
    writeBytesToArchive: write_archive,
    notifyDocumentArchivingProgress: archive_progress,
    notifyDocumentUnarchivingProgress: archive_progress,
    getDocumentArchiveID: archive_id,
};

unsafe extern "C" fn archive_size(
    _controller: ffi::ArchivingControllerHostRef,
    reader: ffi::ArchiveReaderHostRef,
) -> ffi::Size {
    if reader.is_null() {
        return 0;
    }
    // SAFETY: an `ArchiveIn` alive for the restoring call this is made in.
    unsafe { (*(reader as *const ArchiveIn)).data.len() }
}

unsafe extern "C" fn read_archive(
    _controller: ffi::ArchivingControllerHostRef,
    reader: ffi::ArchiveReaderHostRef,
    position: ffi::Size,
    length: ffi::Size,
    buffer: *mut ffi::Byte,
) -> ffi::Bool {
    if reader.is_null() || buffer.is_null() {
        return ffi::FALSE;
    }
    // SAFETY: as above.
    let data = unsafe { &(*(reader as *const ArchiveIn)).data };
    let Some(end) = position.checked_add(length).filter(|&e| e <= data.len()) else {
        tracing::warn!("ARA: a plug-in read past its archive's end");
        return ffi::FALSE;
    };
    // SAFETY: the plug-in's buffer holds `length` bytes.
    unsafe { std::ptr::copy_nonoverlapping(data[position..end].as_ptr(), buffer, length) };
    ffi::TRUE
}

/// Archives larger than this are refused (a broken plug-in writing far
/// past its data).
const MAX_ARCHIVE: usize = 1 << 32;

unsafe extern "C" fn write_archive(
    _controller: ffi::ArchivingControllerHostRef,
    writer: ffi::ArchiveWriterHostRef,
    position: ffi::Size,
    length: ffi::Size,
    buffer: *const ffi::Byte,
) -> ffi::Bool {
    if writer.is_null() || (buffer.is_null() && length > 0) {
        return ffi::FALSE;
    }
    let Some(end) = position.checked_add(length).filter(|&e| e <= MAX_ARCHIVE) else {
        return ffi::FALSE;
    };
    // SAFETY: an `ArchiveOut` alive for the storing call this is made in.
    let out = unsafe { &*(writer as *const ArchiveOut) };
    let Ok(mut data) = out.data.lock() else {
        return ffi::FALSE;
    };
    if data.len() < end {
        // Skipped bytes are zero (ARA's rule).
        data.resize(end, 0);
    }
    if length > 0 {
        // SAFETY: the plug-in's buffer holds `length` bytes.
        let src = unsafe { std::slice::from_raw_parts(buffer, length) };
        data[position..end].copy_from_slice(src);
    }
    ffi::TRUE
}

unsafe extern "C" fn archive_progress(_controller: ffi::ArchivingControllerHostRef, _value: f32) {}

unsafe extern "C" fn archive_id(
    _controller: ffi::ArchivingControllerHostRef,
    reader: ffi::ArchiveReaderHostRef,
) -> ffi::PersistentID {
    if reader.is_null() {
        return std::ptr::null();
    }
    // SAFETY: as in `archive_size`; the string lives as long as the reader.
    unsafe { (*(reader as *const ArchiveIn)).id.as_ptr() }
}

// --- content access (the musical context) ----------------------------------

/// Events of one content reader, in the C layout, with the strings they
/// point to.
enum HostEvents {
    Tempo(Vec<ffi::ContentTempoEntry>),
    Bars(Vec<ffi::ContentBarSignature>),
    /// The names the events point to are kept alive with them.
    Keys(
        Vec<ffi::ContentKeySignature>,
        #[allow(dead_code)] Vec<CString>,
    ),
    Chords(Vec<ffi::ContentChord>, #[allow(dead_code)] Vec<CString>),
}

impl HostEvents {
    fn len(&self) -> usize {
        match self {
            HostEvents::Tempo(v) => v.len(),
            HostEvents::Bars(v) => v.len(),
            HostEvents::Keys(v, _) => v.len(),
            HostEvents::Chords(v, _) => v.len(),
        }
    }

    fn at(&self, i: usize) -> *const c_void {
        let p = match self {
            HostEvents::Tempo(v) => v.get(i).map(|e| e as *const _ as *const c_void),
            HostEvents::Bars(v) => v.get(i).map(|e| e as *const _ as *const c_void),
            HostEvents::Keys(v, _) => v.get(i).map(|e| e as *const _ as *const c_void),
            HostEvents::Chords(v, _) => v.get(i).map(|e| e as *const _ as *const c_void),
        };
        p.unwrap_or(std::ptr::null())
    }
}

/// A name for the C side (null when there is none or it cannot be one).
fn c_name(name: &Option<String>, keep: &mut Vec<CString>) -> *const c_char {
    match name.as_deref().and_then(|n| CString::new(n).ok()) {
        Some(c) => {
            let p = c.as_ptr();
            // The CString's heap buffer does not move with the vector.
            keep.push(c);
            p
        }
        None => std::ptr::null(),
    }
}

fn events_of(context: &Context, kind: ffi::ContentType) -> Option<HostEvents> {
    match kind {
        ffi::kARAContentTypeTempoEntries if context.tempo.len() >= 2 => Some(HostEvents::Tempo(
            context
                .tempo
                .iter()
                .map(|e| ffi::ContentTempoEntry {
                    timePosition: e.seconds,
                    quarterPosition: e.quarters,
                })
                .collect(),
        )),
        // Bars only with a tempo map (ARA's rule for every musical event).
        ffi::kARAContentTypeBarSignatures
            if context.tempo.len() >= 2 && !context.bars.is_empty() =>
        {
            Some(HostEvents::Bars(
                context
                    .bars
                    .iter()
                    .map(|b| ffi::ContentBarSignature {
                        numerator: b.numerator,
                        denominator: b.denominator,
                        position: b.quarters.get(),
                    })
                    .collect(),
            ))
        }
        ffi::kARAContentTypeKeySignatures
            if context.tempo.len() >= 2 && !context.keys.is_empty() =>
        {
            let mut names = Vec::new();
            let events = context
                .keys
                .iter()
                .map(|k| ffi::ContentKeySignature {
                    root: k.root,
                    intervals: k.intervals.map(|on| {
                        if on {
                            ffi::kARAKeySignatureIntervalUsed
                        } else {
                            0
                        }
                    }),
                    name: c_name(&k.name, &mut names),
                    position: k.quarters.get(),
                })
                .collect();
            Some(HostEvents::Keys(events, names))
        }
        ffi::kARAContentTypeSheetChords
            if context.tempo.len() >= 2 && !context.chords.is_empty() =>
        {
            let mut names = Vec::new();
            let events = context
                .chords
                .iter()
                .map(|c| ffi::ContentChord {
                    root: c.root,
                    bass: c.bass,
                    intervals: c.intervals,
                    name: c_name(&c.name, &mut names),
                    position: c.quarters.get(),
                })
                .collect();
            Some(HostEvents::Chords(events, names))
        }
        _ => None,
    }
}

static CONTENT_ACCESS: ffi::ContentAccessControllerInterface =
    ffi::ContentAccessControllerInterface {
        structSize: std::mem::size_of::<ffi::ContentAccessControllerInterface>(),
        isMusicalContextContentAvailable: context_available,
        getMusicalContextContentGrade: context_grade,
        createMusicalContextContentReader: context_reader,
        isAudioSourceContentAvailable: source_available,
        getAudioSourceContentGrade: source_grade,
        createAudioSourceContentReader: source_reader,
        getContentReaderEventCount: reader_count,
        getContentReaderDataForEvent: reader_event,
        destroyContentReader: reader_destroy,
    };

unsafe extern "C" fn context_available(
    controller: ffi::ContentAccessControllerHostRef,
    _context: ffi::MusicalContextHostRef,
    kind: ffi::ContentType,
) -> ffi::Bool {
    // SAFETY: the controller reference of a live document.
    let core = unsafe { core(controller) };
    let ok = core
        .context
        .lock()
        .map(|c| events_of(&c, kind).is_some())
        .unwrap_or(false);
    if ok { ffi::TRUE } else { ffi::FALSE }
}

unsafe extern "C" fn context_grade(
    _controller: ffi::ContentAccessControllerHostRef,
    _context: ffi::MusicalContextHostRef,
    _kind: ffi::ContentType,
) -> ffi::ContentGrade {
    // The project's tempo map, key and chords are the user's.
    ffi::kARAContentGradeAdjusted
}

unsafe extern "C" fn context_reader(
    controller: ffi::ContentAccessControllerHostRef,
    _context: ffi::MusicalContextHostRef,
    kind: ffi::ContentType,
    _range: *const ffi::ContentTimeRange,
) -> ffi::ContentReaderHostRef {
    // SAFETY: as above.
    let core = unsafe { core(controller) };
    // The whole content: a range is only a hint.
    match core.context.lock().ok().and_then(|c| events_of(&c, kind)) {
        Some(events) => Box::into_raw(Box::new(events)) as *mut c_void,
        None => std::ptr::null_mut(),
    }
}

unsafe extern "C" fn source_available(
    _controller: ffi::ContentAccessControllerHostRef,
    _source: ffi::AudioSourceHostRef,
    _kind: ffi::ContentType,
) -> ffi::Bool {
    // FaderFrame knows nothing about a file's content the plug-in would
    // not find better itself.
    ffi::FALSE
}

unsafe extern "C" fn source_grade(
    _controller: ffi::ContentAccessControllerHostRef,
    _source: ffi::AudioSourceHostRef,
    _kind: ffi::ContentType,
) -> ffi::ContentGrade {
    ffi::kARAContentGradeInitial
}

unsafe extern "C" fn source_reader(
    _controller: ffi::ContentAccessControllerHostRef,
    _source: ffi::AudioSourceHostRef,
    _kind: ffi::ContentType,
    _range: *const ffi::ContentTimeRange,
) -> ffi::ContentReaderHostRef {
    std::ptr::null_mut()
}

unsafe extern "C" fn reader_count(
    _controller: ffi::ContentAccessControllerHostRef,
    reader: ffi::ContentReaderHostRef,
) -> ffi::Int32 {
    if reader.is_null() {
        return 0;
    }
    // SAFETY: a reader made by `context_reader`, not yet destroyed.
    let n = unsafe { (*(reader as *const HostEvents)).len() };
    i32::try_from(n).unwrap_or(i32::MAX)
}

unsafe extern "C" fn reader_event(
    _controller: ffi::ContentAccessControllerHostRef,
    reader: ffi::ContentReaderHostRef,
    index: ffi::Int32,
) -> *const c_void {
    if reader.is_null() || index < 0 {
        return std::ptr::null();
    }
    // SAFETY: as above; the event stays valid until the reader goes.
    unsafe { (*(reader as *const HostEvents)).at(index as usize) }
}

unsafe extern "C" fn reader_destroy(
    _controller: ffi::ContentAccessControllerHostRef,
    reader: ffi::ContentReaderHostRef,
) {
    if !reader.is_null() {
        // SAFETY: made by `context_reader`, destroyed once.
        drop(unsafe { Box::from_raw(reader as *mut HostEvents) });
    }
}

// --- model updates -----------------------------------------------------------

static MODEL_UPDATE: ffi::ModelUpdateControllerInterface = ffi::ModelUpdateControllerInterface {
    structSize: std::mem::size_of::<ffi::ModelUpdateControllerInterface>(),
    notifyAudioSourceAnalysisProgress: analysis_progress,
    notifyAudioSourceContentChanged: source_changed,
    notifyAudioModificationContentChanged: modification_changed,
    notifyPlaybackRegionContentChanged: region_changed,
    notifyDocumentDataChanged: document_changed,
};

/// # Safety
/// `r` is a [`SourceHost`] host reference of a live source.
unsafe fn source_key(r: ffi::AudioSourceHostRef) -> String {
    if r.is_null() {
        return String::new();
    }
    // SAFETY: the caller's contract.
    unsafe { (*(r as *const SourceHost)).key.clone() }
}

unsafe extern "C" fn analysis_progress(
    controller: ffi::ModelUpdateControllerHostRef,
    source: ffi::AudioSourceHostRef,
    state: ffi::AnalysisProgressState,
    value: f32,
) {
    // SAFETY: references of a live document.
    let (core, key) = unsafe { (core(controller), source_key(source)) };
    let state = match state {
        ffi::kARAAnalysisProgressStarted => AnalysisState::Started,
        ffi::kARAAnalysisProgressCompleted => AnalysisState::Completed,
        _ => AnalysisState::Updated,
    };
    core.update(|u| {
        u.analysis.retain(|a| a.source != key);
        u.analysis.push(Analysis {
            source: key,
            state,
            progress: value.clamp(0.0, 1.0),
        });
    });
}

fn note_key(list: &mut Vec<String>, key: String) {
    if !list.contains(&key) {
        list.push(key);
    }
}

unsafe extern "C" fn source_changed(
    controller: ffi::ModelUpdateControllerHostRef,
    source: ffi::AudioSourceHostRef,
    _range: *const ffi::ContentTimeRange,
    _flags: ffi::ContentUpdateFlags,
) {
    // SAFETY: as above.
    let (core, key) = unsafe { (core(controller), source_key(source)) };
    core.update(|u| note_key(&mut u.sources, key));
}

unsafe extern "C" fn modification_changed(
    controller: ffi::ModelUpdateControllerHostRef,
    modification: ffi::AudioModificationHostRef,
    _range: *const ffi::ContentTimeRange,
    _flags: ffi::ContentUpdateFlags,
) {
    // SAFETY: as above.
    let (core, key) = unsafe { (core(controller), key_of(modification)) };
    core.update(|u| note_key(&mut u.modifications, key));
}

unsafe extern "C" fn region_changed(
    controller: ffi::ModelUpdateControllerHostRef,
    region: ffi::PlaybackRegionHostRef,
    _range: *const ffi::ContentTimeRange,
    _flags: ffi::ContentUpdateFlags,
) {
    // SAFETY: as above.
    let (core, key) = unsafe { (core(controller), key_of(region)) };
    core.update(|u| note_key(&mut u.regions, key));
}

unsafe extern "C" fn document_changed(controller: ffi::ModelUpdateControllerHostRef) {
    // SAFETY: as above.
    let core = unsafe { core(controller) };
    core.update(|u| u.document = true);
}

// --- playback requests -------------------------------------------------------

static PLAYBACK: ffi::PlaybackControllerInterface = ffi::PlaybackControllerInterface {
    structSize: std::mem::size_of::<ffi::PlaybackControllerInterface>(),
    requestStartPlayback: request_start,
    requestStopPlayback: request_stop,
    requestSetPlaybackPosition: request_locate,
    requestSetCycleRange: request_cycle_range,
    requestEnableCycle: request_enable_cycle,
};

fn request(controller: ffi::PlaybackControllerHostRef, r: PlaybackRequest) {
    // SAFETY: the controller reference of a live document (these may come
    // from any non-render thread: the updates sit behind a mutex).
    let core = unsafe { core(controller) };
    core.update(|u| u.playback.push(r));
}

unsafe extern "C" fn request_start(controller: ffi::PlaybackControllerHostRef) {
    request(controller, PlaybackRequest::Start);
}

unsafe extern "C" fn request_stop(controller: ffi::PlaybackControllerHostRef) {
    request(controller, PlaybackRequest::Stop);
}

unsafe extern "C" fn request_locate(
    controller: ffi::PlaybackControllerHostRef,
    seconds: ffi::TimePosition,
) {
    if seconds.is_finite() {
        request(controller, PlaybackRequest::Locate(seconds));
    }
}

unsafe extern "C" fn request_cycle_range(
    controller: ffi::PlaybackControllerHostRef,
    start: ffi::TimePosition,
    duration: ffi::TimeDuration,
) {
    if start.is_finite() && duration.is_finite() && duration > 0.0 {
        request(controller, PlaybackRequest::CycleRange { start, duration });
    }
}

unsafe extern "C" fn request_enable_cycle(
    controller: ffi::PlaybackControllerHostRef,
    enable: ffi::Bool,
) {
    request(
        controller,
        PlaybackRequest::EnableCycle(enable != ffi::FALSE),
    );
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    /// Sample k of every channel is k (+ 0.5 on the second).
    struct Ramp;

    impl Samples for Ramp {
        fn read(&self, start: i64, out: &mut [&mut [f32]]) -> bool {
            for (c, o) in out.iter_mut().enumerate() {
                for (i, v) in o.iter_mut().enumerate() {
                    *v = (start + i as i64) as f32 + c as f32 * 0.5;
                }
            }
            true
        }
    }

    #[test]
    fn readers_read_32_and_64_bit_samples_in_chunks() {
        let core = HostCore::new();
        let source = Box::new(SourceHost {
            key: "s".into(),
            channels: 2,
            samples: Mutex::new(Arc::new(Ramp)),
        });
        let src = &*source as *const SourceHost as *mut c_void;
        let ctrl = core.instance.audioAccessControllerHostRef;
        let n = SCRATCH_FRAMES + 1000;
        // SAFETY: a live source; buffers of `n` samples per channel.
        unsafe {
            let r = create_audio_reader(ctrl, src, ffi::TRUE);
            let (mut a, mut b) = (vec![0f64; n], vec![0f64; n]);
            let p = [a.as_mut_ptr() as *mut c_void, b.as_mut_ptr() as *mut c_void];
            assert_eq!(
                read_audio_samples(ctrl, r, 10, n as i64, p.as_ptr()),
                ffi::TRUE
            );
            assert_eq!(a[0], 10.0);
            assert_eq!(b[n - 1], (n + 9) as f64 + 0.5);
            destroy_audio_reader(ctrl, r);
            let r = create_audio_reader(ctrl, src, ffi::FALSE);
            let (mut a, mut b) = (vec![0f32; 8], vec![0f32; 8]);
            let p = [a.as_mut_ptr() as *mut c_void, b.as_mut_ptr() as *mut c_void];
            assert_eq!(read_audio_samples(ctrl, r, -4, 8, p.as_ptr()), ffi::TRUE);
            assert_eq!(a[4], 0.0);
            assert_eq!(b[7], 3.5);
            destroy_audio_reader(ctrl, r);
        }
    }

    #[test]
    fn archives_are_written_at_positions_and_read_in_bounds() {
        let out = ArchiveOut {
            data: Mutex::new(Vec::new()),
        };
        let w = &out as *const ArchiveOut as *mut c_void;
        // SAFETY: a writer and a reader alive for the calls.
        unsafe {
            assert_eq!(
                write_archive(std::ptr::null_mut(), w, 4, 2, b"cd".as_ptr()),
                ffi::TRUE
            );
            assert_eq!(
                write_archive(std::ptr::null_mut(), w, 0, 2, b"ab".as_ptr()),
                ffi::TRUE
            );
        }
        assert_eq!(*out.data.lock().unwrap(), b"ab\0\0cd");
        let input = ArchiveIn {
            data: b"hello".to_vec(),
            id: CString::new("id.1").unwrap(),
        };
        let r = &input as *const ArchiveIn as *mut c_void;
        let mut buf = [0u8; 3];
        // SAFETY: as above.
        unsafe {
            assert_eq!(archive_size(std::ptr::null_mut(), r), 5);
            assert_eq!(
                read_archive(std::ptr::null_mut(), r, 2, 3, buf.as_mut_ptr()),
                ffi::TRUE
            );
            assert_eq!(
                read_archive(std::ptr::null_mut(), r, 4, 3, buf.as_mut_ptr()),
                ffi::FALSE
            );
            assert_eq!(
                crate::content::text(archive_id(std::ptr::null_mut(), r)).unwrap(),
                "id.1"
            );
        }
        assert_eq!(&buf, b"llo");
    }
}
