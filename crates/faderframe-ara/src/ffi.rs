//! The ARA 2 C API (`ARAInterface.h` of ARA SDK 2.3.0), translated by hand.
//!
//! Only what a host uses is here: the model graph's property structs,
//! content events, the five host interfaces, the document controller, the
//! factory and the plug-in extension. Names follow the header without the
//! `ARA` prefix; constants keep theirs. The header packs its structs to 1
//! byte on x86 and to 8 bytes (natural alignment for everything in it) on
//! ARM, so every struct is declared through [`ara_struct!`], which picks the
//! matching `repr`. Packed fields are read by value (`{ s.field }`), never
//! by reference. The layouts are checked against offsets worked out from
//! the header in the tests below.
//!
//! The ARA API is Apache-2.0 (Celemony Software GmbH); see
//! `vendor/VENDORED.md`.
#![allow(
    non_snake_case,
    non_camel_case_types,
    non_upper_case_globals,
    clippy::upper_case_acronyms
)]

use std::ffi::{c_char, c_void};

/// Declare an ARA struct with the header's packing (for structs whose
/// fields the packing moves).
macro_rules! ara_struct {
    ($(#[$m:meta])* pub struct $name:ident { $($(#[$fm:meta])* pub $f:ident : $t:ty),* $(,)? }) => {
        $(#[$m])*
        #[cfg_attr(any(target_arch = "x86_64", target_arch = "x86"), repr(C, packed))]
        #[cfg_attr(not(any(target_arch = "x86_64", target_arch = "x86")), repr(C))]
        #[derive(Clone, Copy)]
        pub struct $name { $($(#[$fm])* pub $f: $t),* }
    };
}

/// Declare an ARA struct whose packed layout is its natural one (every
/// field at a multiple of its own alignment: pointer tables, pairs of
/// 32-bit values): plain `repr(C)`, so its `Option` fields can be used in
/// place. Plug-ins' copies are read with `read_unaligned` all the same.
macro_rules! ara_table {
    ($(#[$m:meta])* pub struct $name:ident { $($(#[$fm:meta])* pub $f:ident : $t:ty),* $(,)? }) => {
        $(#[$m])*
        #[repr(C)]
        #[derive(Clone, Copy)]
        pub struct $name { $($(#[$fm])* pub $f: $t),* }
    };
}

pub type Byte = u8;
pub type Int32 = i32;
pub type Int64 = i64;
pub type Size = usize;
/// 32-bit boolean (`kARATrue`/`kARAFalse`).
pub type Bool = i32;
pub const TRUE: Bool = 1;
pub const FALSE: Bool = 0;
pub type Utf8String = *const c_char;
pub type PersistentID = *const c_char;
pub type TimePosition = f64;
pub type TimeDuration = f64;
pub type SamplePosition = i64;
pub type SampleCount = i64;
pub type QuarterPosition = f64;
pub type SampleRate = f64;
pub type ChannelCount = i32;

// Opaque references: the plug-in's (`…Ref`) and the host's (`…HostRef`).
pub type MusicalContextRef = *mut c_void;
pub type MusicalContextHostRef = *mut c_void;
pub type RegionSequenceRef = *mut c_void;
pub type RegionSequenceHostRef = *mut c_void;
pub type AudioSourceRef = *mut c_void;
pub type AudioSourceHostRef = *mut c_void;
pub type AudioModificationRef = *mut c_void;
pub type AudioModificationHostRef = *mut c_void;
pub type PlaybackRegionRef = *mut c_void;
pub type PlaybackRegionHostRef = *mut c_void;
pub type ContentReaderRef = *mut c_void;
pub type ContentReaderHostRef = *mut c_void;
pub type DocumentControllerRef = *mut c_void;
pub type AudioAccessControllerHostRef = *mut c_void;
pub type AudioReaderHostRef = *mut c_void;
pub type ArchivingControllerHostRef = *mut c_void;
pub type ArchiveReaderHostRef = *mut c_void;
pub type ArchiveWriterHostRef = *mut c_void;
pub type ContentAccessControllerHostRef = *mut c_void;
pub type ModelUpdateControllerHostRef = *mut c_void;
pub type PlaybackControllerHostRef = *mut c_void;
pub type PlaybackRendererRef = *mut c_void;
pub type EditorRendererRef = *mut c_void;
pub type EditorViewRef = *mut c_void;
pub type PlugInExtensionRef = *mut c_void;

// --- enums (all 32-bit) ---------------------------------------------------

pub type APIGeneration = i32;
pub const kARAAPIGeneration_2_0_Draft: APIGeneration = 3;
pub const kARAAPIGeneration_2_0_Final: APIGeneration = 4;
pub const kARAAPIGeneration_2_X_Draft: APIGeneration = 5;
pub const kARAAPIGeneration_2_3_Final: APIGeneration = 6;

pub type AssertCategory = i32;
pub const kARAAssertUnspecified: AssertCategory = 0;
pub const kARAAssertInvalidArgument: AssertCategory = -1;
pub const kARAAssertInvalidState: AssertCategory = -2;
pub const kARAAssertInvalidThread: AssertCategory = -3;

pub type ChannelArrangementDataType = i32;
pub const kARAChannelArrangementUndefined: ChannelArrangementDataType = 0;

pub type PlaybackTransformationFlags = i32;
pub const kARAPlaybackTransformationNoChanges: PlaybackTransformationFlags = 0;
pub const kARAPlaybackTransformationTimestretch: PlaybackTransformationFlags = 1 << 0;
pub const kARAPlaybackTransformationTimestretchReflectingTempo: PlaybackTransformationFlags =
    1 << 1;
pub const kARAPlaybackTransformationContentBasedFadeAtTail: PlaybackTransformationFlags = 1 << 2;
pub const kARAPlaybackTransformationContentBasedFadeAtHead: PlaybackTransformationFlags = 1 << 3;

pub type ContentUpdateFlags = i32;
pub const kARAContentUpdateEverythingChanged: ContentUpdateFlags = 0;
pub const kARAContentUpdateSignalScopeRemainsUnchanged: ContentUpdateFlags = 1 << 0;
pub const kARAContentUpdateNoteScopeRemainsUnchanged: ContentUpdateFlags = 1 << 1;
pub const kARAContentUpdateTimingScopeRemainsUnchanged: ContentUpdateFlags = 1 << 2;
pub const kARAContentUpdateTuningScopeRemainsUnchanged: ContentUpdateFlags = 1 << 3;
pub const kARAContentUpdateHarmonicScopeRemainsUnchanged: ContentUpdateFlags = 1 << 4;

pub type ContentType = i32;
pub const kARAContentTypeNotes: ContentType = 10;
pub const kARAContentTypeTempoEntries: ContentType = 20;
pub const kARAContentTypeBarSignatures: ContentType = 21;
pub const kARAContentTypeStaticTuning: ContentType = 31;
pub const kARAContentTypeKeySignatures: ContentType = 42;
pub const kARAContentTypeSheetChords: ContentType = 45;

pub type ContentGrade = i32;
pub const kARAContentGradeInitial: ContentGrade = 0;
pub const kARAContentGradeDetected: ContentGrade = 1;
pub const kARAContentGradeAdjusted: ContentGrade = 2;
pub const kARAContentGradeApproved: ContentGrade = 3;

pub type AnalysisProgressState = i32;
pub const kARAAnalysisProgressStarted: AnalysisProgressState = 0;
pub const kARAAnalysisProgressUpdated: AnalysisProgressState = 1;
pub const kARAAnalysisProgressCompleted: AnalysisProgressState = 2;

pub type PlugInInstanceRoleFlags = i32;
pub const kARAPlaybackRendererRole: PlugInInstanceRoleFlags = 1 << 0;
pub const kARAEditorRendererRole: PlugInInstanceRoleFlags = 1 << 1;
pub const kARAEditorViewRole: PlugInInstanceRoleFlags = 1 << 2;

/// A note without a defined pitch (percussive).
pub const kARAInvalidPitchNumber: i32 = i32::MIN;
pub const kARAKeySignatureIntervalUsed: u8 = 0xFF;
pub const kARAChordIntervalUsed: u8 = 0xFF;

pub type AssertFunction = unsafe extern "C" fn(
    category: AssertCategory,
    problematic_argument: *const c_void,
    diagnosis: *const c_char,
);

// --- model graph properties ----------------------------------------------

ara_table! {
    pub struct Color { pub r: f32, pub g: f32, pub b: f32 }
}

ara_table! {
    pub struct DocumentProperties {
        pub structSize: Size,
        pub name: Utf8String,
    }
}

ara_struct! {
    pub struct MusicalContextProperties {
        pub structSize: Size,
        pub name: Utf8String,
        pub orderIndex: Int32,
        pub color: *const Color,
    }
}

ara_struct! {
    pub struct RegionSequenceProperties {
        pub structSize: Size,
        pub name: Utf8String,
        pub orderIndex: Int32,
        pub musicalContextRef: MusicalContextRef,
        pub color: *const Color,
    }
}

ara_struct! {
    pub struct AudioSourceProperties {
        pub structSize: Size,
        pub name: Utf8String,
        pub persistentID: PersistentID,
        pub sampleCount: SampleCount,
        pub sampleRate: SampleRate,
        pub channelCount: ChannelCount,
        pub merits64BitSamples: Bool,
        pub channelArrangementDataType: ChannelArrangementDataType,
        pub channelArrangement: *const c_void,
    }
}

ara_table! {
    pub struct AudioModificationProperties {
        pub structSize: Size,
        pub name: Utf8String,
        pub persistentID: PersistentID,
    }
}

ara_struct! {
    pub struct PlaybackRegionProperties {
        pub structSize: Size,
        pub transformationFlags: PlaybackTransformationFlags,
        pub startInModificationTime: TimePosition,
        pub durationInModificationTime: TimeDuration,
        pub startInPlaybackTime: TimePosition,
        pub durationInPlaybackTime: TimeDuration,
        /// Deprecated since 2.0 (region sequences carry the context).
        pub musicalContextRef: MusicalContextRef,
        pub regionSequenceRef: RegionSequenceRef,
        pub name: Utf8String,
        pub color: *const Color,
    }
}

// --- content ---------------------------------------------------------------

ara_table! {
    pub struct ContentTimeRange {
        pub start: TimePosition,
        pub duration: TimeDuration,
    }
}

ara_table! {
    pub struct ContentTempoEntry {
        pub timePosition: TimePosition,
        pub quarterPosition: QuarterPosition,
    }
}

ara_table! {
    pub struct ContentBarSignature {
        pub numerator: Int32,
        pub denominator: Int32,
        pub position: QuarterPosition,
    }
}

ara_struct! {
    pub struct ContentNote {
        pub frequency: f32,
        pub pitchNumber: Int32,
        pub volume: f32,
        pub startPosition: TimePosition,
        pub attackDuration: TimeDuration,
        pub noteDuration: TimeDuration,
        pub signalDuration: TimeDuration,
    }
}

ara_table! {
    pub struct ContentTuning {
        pub concertPitchFrequency: f32,
        pub root: Int32,
        pub tunings: [f32; 12],
        pub name: Utf8String,
    }
}

ara_table! {
    pub struct ContentKeySignature {
        /// Circle-of-fifths index from C.
        pub root: Int32,
        pub intervals: [Byte; 12],
        pub name: Utf8String,
        pub position: QuarterPosition,
    }
}

ara_struct! {
    pub struct ContentChord {
        pub root: Int32,
        pub bass: Int32,
        pub intervals: [Byte; 12],
        pub name: Utf8String,
        pub position: QuarterPosition,
    }
}

// --- host interfaces ------------------------------------------------------

ara_table! {
    pub struct AudioAccessControllerInterface {
        pub structSize: Size,
        pub createAudioReaderForSource: unsafe extern "C" fn(
            AudioAccessControllerHostRef,
            AudioSourceHostRef,
            Bool,
        ) -> AudioReaderHostRef,
        pub readAudioSamples: unsafe extern "C" fn(
            AudioAccessControllerHostRef,
            AudioReaderHostRef,
            SamplePosition,
            SampleCount,
            *const *mut c_void,
        ) -> Bool,
        pub destroyAudioReader: unsafe extern "C" fn(AudioAccessControllerHostRef, AudioReaderHostRef),
    }
}

ara_table! {
    pub struct ArchivingControllerInterface {
        pub structSize: Size,
        pub getArchiveSize: unsafe extern "C" fn(ArchivingControllerHostRef, ArchiveReaderHostRef) -> Size,
        pub readBytesFromArchive: unsafe extern "C" fn(
            ArchivingControllerHostRef,
            ArchiveReaderHostRef,
            Size,
            Size,
            *mut Byte,
        ) -> Bool,
        pub writeBytesToArchive: unsafe extern "C" fn(
            ArchivingControllerHostRef,
            ArchiveWriterHostRef,
            Size,
            Size,
            *const Byte,
        ) -> Bool,
        pub notifyDocumentArchivingProgress: unsafe extern "C" fn(ArchivingControllerHostRef, f32),
        pub notifyDocumentUnarchivingProgress: unsafe extern "C" fn(ArchivingControllerHostRef, f32),
        pub getDocumentArchiveID: unsafe extern "C" fn(ArchivingControllerHostRef, ArchiveReaderHostRef) -> PersistentID,
    }
}

ara_table! {
    pub struct ContentAccessControllerInterface {
        pub structSize: Size,
        pub isMusicalContextContentAvailable: unsafe extern "C" fn(
            ContentAccessControllerHostRef,
            MusicalContextHostRef,
            ContentType,
        ) -> Bool,
        pub getMusicalContextContentGrade: unsafe extern "C" fn(
            ContentAccessControllerHostRef,
            MusicalContextHostRef,
            ContentType,
        ) -> ContentGrade,
        pub createMusicalContextContentReader: unsafe extern "C" fn(
            ContentAccessControllerHostRef,
            MusicalContextHostRef,
            ContentType,
            *const ContentTimeRange,
        ) -> ContentReaderHostRef,
        pub isAudioSourceContentAvailable: unsafe extern "C" fn(
            ContentAccessControllerHostRef,
            AudioSourceHostRef,
            ContentType,
        ) -> Bool,
        pub getAudioSourceContentGrade: unsafe extern "C" fn(
            ContentAccessControllerHostRef,
            AudioSourceHostRef,
            ContentType,
        ) -> ContentGrade,
        pub createAudioSourceContentReader: unsafe extern "C" fn(
            ContentAccessControllerHostRef,
            AudioSourceHostRef,
            ContentType,
            *const ContentTimeRange,
        ) -> ContentReaderHostRef,
        pub getContentReaderEventCount: unsafe extern "C" fn(ContentAccessControllerHostRef, ContentReaderHostRef) -> Int32,
        pub getContentReaderDataForEvent: unsafe extern "C" fn(
            ContentAccessControllerHostRef,
            ContentReaderHostRef,
            Int32,
        ) -> *const c_void,
        pub destroyContentReader: unsafe extern "C" fn(ContentAccessControllerHostRef, ContentReaderHostRef),
    }
}

ara_table! {
    pub struct ModelUpdateControllerInterface {
        pub structSize: Size,
        pub notifyAudioSourceAnalysisProgress: unsafe extern "C" fn(
            ModelUpdateControllerHostRef,
            AudioSourceHostRef,
            AnalysisProgressState,
            f32,
        ),
        pub notifyAudioSourceContentChanged: unsafe extern "C" fn(
            ModelUpdateControllerHostRef,
            AudioSourceHostRef,
            *const ContentTimeRange,
            ContentUpdateFlags,
        ),
        pub notifyAudioModificationContentChanged: unsafe extern "C" fn(
            ModelUpdateControllerHostRef,
            AudioModificationHostRef,
            *const ContentTimeRange,
            ContentUpdateFlags,
        ),
        pub notifyPlaybackRegionContentChanged: unsafe extern "C" fn(
            ModelUpdateControllerHostRef,
            PlaybackRegionHostRef,
            *const ContentTimeRange,
            ContentUpdateFlags,
        ),
        pub notifyDocumentDataChanged: unsafe extern "C" fn(ModelUpdateControllerHostRef),
    }
}

ara_table! {
    pub struct PlaybackControllerInterface {
        pub structSize: Size,
        pub requestStartPlayback: unsafe extern "C" fn(PlaybackControllerHostRef),
        pub requestStopPlayback: unsafe extern "C" fn(PlaybackControllerHostRef),
        pub requestSetPlaybackPosition: unsafe extern "C" fn(PlaybackControllerHostRef, TimePosition),
        pub requestSetCycleRange: unsafe extern "C" fn(PlaybackControllerHostRef, TimePosition, TimeDuration),
        pub requestEnableCycle: unsafe extern "C" fn(PlaybackControllerHostRef, Bool),
    }
}

ara_table! {
    pub struct DocumentControllerHostInstance {
        pub structSize: Size,
        pub audioAccessControllerHostRef: AudioAccessControllerHostRef,
        pub audioAccessControllerInterface: *const AudioAccessControllerInterface,
        pub archivingControllerHostRef: ArchivingControllerHostRef,
        pub archivingControllerInterface: *const ArchivingControllerInterface,
        pub contentAccessControllerHostRef: ContentAccessControllerHostRef,
        pub contentAccessControllerInterface: *const ContentAccessControllerInterface,
        pub modelUpdateControllerHostRef: ModelUpdateControllerHostRef,
        pub modelUpdateControllerInterface: *const ModelUpdateControllerInterface,
        pub playbackControllerHostRef: PlaybackControllerHostRef,
        pub playbackControllerInterface: *const PlaybackControllerInterface,
    }
}

// --- partial persistency ----------------------------------------------------

ara_struct! {
    pub struct RestoreObjectsFilter {
        pub structSize: Size,
        pub documentData: Bool,
        pub audioSourceIDsCount: Size,
        pub audioSourceArchiveIDs: *const PersistentID,
        pub audioSourceCurrentIDs: *const PersistentID,
        pub audioModificationIDsCount: Size,
        pub audioModificationArchiveIDs: *const PersistentID,
        pub audioModificationCurrentIDs: *const PersistentID,
    }
}

ara_struct! {
    pub struct StoreObjectsFilter {
        pub structSize: Size,
        pub documentData: Bool,
        pub audioSourceRefsCount: Size,
        pub audioSourceRefs: *const AudioSourceRef,
        pub audioModificationRefsCount: Size,
        pub audioModificationRefs: *const AudioModificationRef,
    }
}

ara_table! {
    pub struct ProcessingAlgorithmProperties {
        pub structSize: Size,
        pub persistentID: PersistentID,
        pub name: Utf8String,
    }
}

// --- the plug-in's document controller ----------------------------------

type Dc = DocumentControllerRef;

ara_table! {
    /// Every entry may be missing past the plug-in's `structSize` (checked
    /// with [`implements`]); `Option` also covers null entries.
    pub struct DocumentControllerInterface {
        pub structSize: Size,
        pub destroyDocumentController: Option<unsafe extern "C" fn(Dc)>,
        pub getFactory: Option<unsafe extern "C" fn(Dc) -> *const Factory>,
        pub beginEditing: Option<unsafe extern "C" fn(Dc)>,
        pub endEditing: Option<unsafe extern "C" fn(Dc)>,
        pub notifyModelUpdates: Option<unsafe extern "C" fn(Dc)>,
        pub beginRestoringDocumentFromArchive: Option<unsafe extern "C" fn(Dc, ArchiveReaderHostRef) -> Bool>,
        pub endRestoringDocumentFromArchive: Option<unsafe extern "C" fn(Dc, ArchiveReaderHostRef) -> Bool>,
        pub storeDocumentToArchive: Option<unsafe extern "C" fn(Dc, ArchiveWriterHostRef) -> Bool>,
        pub updateDocumentProperties: Option<unsafe extern "C" fn(Dc, *const DocumentProperties)>,
        pub createMusicalContext: Option<unsafe extern "C" fn(
            Dc,
            MusicalContextHostRef,
            *const MusicalContextProperties,
        ) -> MusicalContextRef>,
        pub updateMusicalContextProperties: Option<unsafe extern "C" fn(
            Dc,
            MusicalContextRef,
            *const MusicalContextProperties,
        )>,
        pub updateMusicalContextContent: Option<unsafe extern "C" fn(
            Dc,
            MusicalContextRef,
            *const ContentTimeRange,
            ContentUpdateFlags,
        )>,
        pub destroyMusicalContext: Option<unsafe extern "C" fn(Dc, MusicalContextRef)>,
        pub createAudioSource: Option<unsafe extern "C" fn(
            Dc,
            AudioSourceHostRef,
            *const AudioSourceProperties,
        ) -> AudioSourceRef>,
        pub updateAudioSourceProperties: Option<unsafe extern "C" fn(
            Dc,
            AudioSourceRef,
            *const AudioSourceProperties,
        )>,
        pub updateAudioSourceContent: Option<unsafe extern "C" fn(
            Dc,
            AudioSourceRef,
            *const ContentTimeRange,
            ContentUpdateFlags,
        )>,
        pub enableAudioSourceSamplesAccess: Option<unsafe extern "C" fn(Dc, AudioSourceRef, Bool)>,
        pub deactivateAudioSourceForUndoHistory: Option<unsafe extern "C" fn(Dc, AudioSourceRef, Bool)>,
        pub destroyAudioSource: Option<unsafe extern "C" fn(Dc, AudioSourceRef)>,
        pub createAudioModification: Option<unsafe extern "C" fn(
            Dc,
            AudioSourceRef,
            AudioModificationHostRef,
            *const AudioModificationProperties,
        ) -> AudioModificationRef>,
        pub cloneAudioModification: Option<unsafe extern "C" fn(
            Dc,
            AudioModificationRef,
            AudioModificationHostRef,
            *const AudioModificationProperties,
        ) -> AudioModificationRef>,
        pub updateAudioModificationProperties: Option<unsafe extern "C" fn(
            Dc,
            AudioModificationRef,
            *const AudioModificationProperties,
        )>,
        pub deactivateAudioModificationForUndoHistory: Option<unsafe extern "C" fn(Dc, AudioModificationRef, Bool)>,
        pub destroyAudioModification: Option<unsafe extern "C" fn(Dc, AudioModificationRef)>,
        pub createPlaybackRegion: Option<unsafe extern "C" fn(
            Dc,
            AudioModificationRef,
            PlaybackRegionHostRef,
            *const PlaybackRegionProperties,
        ) -> PlaybackRegionRef>,
        pub updatePlaybackRegionProperties: Option<unsafe extern "C" fn(
            Dc,
            PlaybackRegionRef,
            *const PlaybackRegionProperties,
        )>,
        pub destroyPlaybackRegion: Option<unsafe extern "C" fn(Dc, PlaybackRegionRef)>,
        pub isAudioSourceContentAvailable: Option<unsafe extern "C" fn(Dc, AudioSourceRef, ContentType) -> Bool>,
        pub isAudioSourceContentAnalysisIncomplete: Option<unsafe extern "C" fn(Dc, AudioSourceRef, ContentType) -> Bool>,
        pub requestAudioSourceContentAnalysis: Option<unsafe extern "C" fn(
            Dc,
            AudioSourceRef,
            Size,
            *const ContentType,
        )>,
        pub getAudioSourceContentGrade: Option<unsafe extern "C" fn(Dc, AudioSourceRef, ContentType) -> ContentGrade>,
        pub createAudioSourceContentReader: Option<unsafe extern "C" fn(
            Dc,
            AudioSourceRef,
            ContentType,
            *const ContentTimeRange,
        ) -> ContentReaderRef>,
        pub isAudioModificationContentAvailable: Option<unsafe extern "C" fn(Dc, AudioModificationRef, ContentType) -> Bool>,
        pub getAudioModificationContentGrade: Option<unsafe extern "C" fn(Dc, AudioModificationRef, ContentType) -> ContentGrade>,
        pub createAudioModificationContentReader: Option<unsafe extern "C" fn(
            Dc,
            AudioModificationRef,
            ContentType,
            *const ContentTimeRange,
        ) -> ContentReaderRef>,
        pub isPlaybackRegionContentAvailable: Option<unsafe extern "C" fn(Dc, PlaybackRegionRef, ContentType) -> Bool>,
        pub getPlaybackRegionContentGrade: Option<unsafe extern "C" fn(Dc, PlaybackRegionRef, ContentType) -> ContentGrade>,
        pub createPlaybackRegionContentReader: Option<unsafe extern "C" fn(
            Dc,
            PlaybackRegionRef,
            ContentType,
            *const ContentTimeRange,
        ) -> ContentReaderRef>,
        pub getContentReaderEventCount: Option<unsafe extern "C" fn(Dc, ContentReaderRef) -> Int32>,
        pub getContentReaderDataForEvent: Option<unsafe extern "C" fn(Dc, ContentReaderRef, Int32) -> *const c_void>,
        pub destroyContentReader: Option<unsafe extern "C" fn(Dc, ContentReaderRef)>,
        pub createRegionSequence: Option<unsafe extern "C" fn(
            Dc,
            RegionSequenceHostRef,
            *const RegionSequenceProperties,
        ) -> RegionSequenceRef>,
        pub updateRegionSequenceProperties: Option<unsafe extern "C" fn(
            Dc,
            RegionSequenceRef,
            *const RegionSequenceProperties,
        )>,
        pub destroyRegionSequence: Option<unsafe extern "C" fn(Dc, RegionSequenceRef)>,
        pub getPlaybackRegionHeadAndTailTime: Option<unsafe extern "C" fn(
            Dc,
            PlaybackRegionRef,
            *mut TimeDuration,
            *mut TimeDuration,
        )>,
        pub restoreObjectsFromArchive: Option<unsafe extern "C" fn(
            Dc,
            ArchiveReaderHostRef,
            *const RestoreObjectsFilter,
        ) -> Bool>,
        pub storeObjectsToArchive: Option<unsafe extern "C" fn(
            Dc,
            ArchiveWriterHostRef,
            *const StoreObjectsFilter,
        ) -> Bool>,
        pub getProcessingAlgorithmsCount: Option<unsafe extern "C" fn(Dc) -> Int32>,
        pub getProcessingAlgorithmProperties: Option<unsafe extern "C" fn(Dc, Int32) -> *const ProcessingAlgorithmProperties>,
        pub getProcessingAlgorithmForAudioSource: Option<unsafe extern "C" fn(Dc, AudioSourceRef) -> Int32>,
        pub requestProcessingAlgorithmForAudioSource: Option<unsafe extern "C" fn(Dc, AudioSourceRef, Int32)>,
        pub isLicensedForCapabilities: Option<unsafe extern "C" fn(
            Dc,
            Bool,
            Size,
            *const ContentType,
            PlaybackTransformationFlags,
        ) -> Bool>,
        pub storeAudioSourceToAudioFileChunk: Option<unsafe extern "C" fn(
            Dc,
            ArchiveWriterHostRef,
            AudioSourceRef,
            *mut PersistentID,
            *mut Bool,
        ) -> Bool>,
        pub isAudioModificationPreservingAudioSourceSignal: Option<unsafe extern "C" fn(Dc, AudioModificationRef) -> Bool>,
    }
}

ara_table! {
    pub struct DocumentControllerInstance {
        pub structSize: Size,
        pub documentControllerRef: DocumentControllerRef,
        pub documentControllerInterface: *const DocumentControllerInterface,
    }
}

// --- the factory -----------------------------------------------------------

ara_struct! {
    pub struct InterfaceConfiguration {
        pub structSize: Size,
        pub desiredApiGeneration: APIGeneration,
        /// A pointer to the assert function's address (which may be null).
        pub assertFunctionAddress: *const Option<AssertFunction>,
    }
}

ara_table! {
    pub struct Factory {
        pub structSize: Size,
        pub lowestSupportedApiGeneration: APIGeneration,
        pub highestSupportedApiGeneration: APIGeneration,
        pub factoryID: PersistentID,
        pub initializeARAWithConfiguration: Option<unsafe extern "C" fn(*const InterfaceConfiguration)>,
        pub uninitializeARA: Option<unsafe extern "C" fn()>,
        pub plugInName: Utf8String,
        pub manufacturerName: Utf8String,
        pub informationURL: Utf8String,
        pub version: Utf8String,
        pub createDocumentControllerWithDocument: Option<unsafe extern "C" fn(
            *const DocumentControllerHostInstance,
            *const DocumentProperties,
        ) -> *const DocumentControllerInstance>,
        pub documentArchiveID: PersistentID,
        pub compatibleDocumentArchiveIDsCount: Size,
        pub compatibleDocumentArchiveIDs: *const PersistentID,
        pub analyzeableContentTypesCount: Size,
        pub analyzeableContentTypes: *const ContentType,
        pub supportedPlaybackTransformationFlags: PlaybackTransformationFlags,
        pub supportsStoringAudioFileChunks: Bool,
    }
}

// --- the plug-in extension --------------------------------------------------

ara_table! {
    pub struct PlaybackRendererInterface {
        pub structSize: Size,
        pub addPlaybackRegion: Option<unsafe extern "C" fn(PlaybackRendererRef, PlaybackRegionRef)>,
        pub removePlaybackRegion: Option<unsafe extern "C" fn(PlaybackRendererRef, PlaybackRegionRef)>,
    }
}

ara_table! {
    pub struct EditorRendererInterface {
        pub structSize: Size,
        pub addPlaybackRegion: Option<unsafe extern "C" fn(EditorRendererRef, PlaybackRegionRef)>,
        pub removePlaybackRegion: Option<unsafe extern "C" fn(EditorRendererRef, PlaybackRegionRef)>,
        pub addRegionSequence: Option<unsafe extern "C" fn(EditorRendererRef, RegionSequenceRef)>,
        pub removeRegionSequence: Option<unsafe extern "C" fn(EditorRendererRef, RegionSequenceRef)>,
    }
}

ara_table! {
    pub struct ViewSelection {
        pub structSize: Size,
        pub playbackRegionRefsCount: Size,
        pub playbackRegionRefs: *const PlaybackRegionRef,
        pub regionSequenceRefsCount: Size,
        pub regionSequenceRefs: *const RegionSequenceRef,
        pub timeRange: *const ContentTimeRange,
    }
}

ara_table! {
    pub struct EditorViewInterface {
        pub structSize: Size,
        pub notifySelection: Option<unsafe extern "C" fn(EditorViewRef, *const ViewSelection)>,
        pub notifyHideRegionSequences: Option<unsafe extern "C" fn(
            EditorViewRef,
            Size,
            *const RegionSequenceRef,
        )>,
    }
}

ara_table! {
    /// ARA 1's single interface (deprecated; kept for the layout).
    pub struct PlugInExtensionInterface {
        pub structSize: Size,
        pub setPlaybackRegion: Option<unsafe extern "C" fn(PlugInExtensionRef, PlaybackRegionRef)>,
        pub removePlaybackRegion: Option<unsafe extern "C" fn(PlugInExtensionRef, PlaybackRegionRef)>,
    }
}

ara_table! {
    pub struct PlugInExtensionInstance {
        pub structSize: Size,
        pub plugInExtensionRef: PlugInExtensionRef,
        pub plugInExtensionInterface: *const PlugInExtensionInterface,
        pub playbackRendererRef: PlaybackRendererRef,
        pub playbackRendererInterface: *const PlaybackRendererInterface,
        pub editorRendererRef: EditorRendererRef,
        pub editorRendererInterface: *const EditorRendererInterface,
        pub editorViewRef: EditorViewRef,
        pub editorViewInterface: *const EditorViewInterface,
    }
}

/// Whether a versioned struct whose `structSize` is `size` has the field
/// that starts at `offset` (`ARA_IMPLEMENTS_FIELD`).
#[inline]
pub fn implements(size: Size, offset: usize) -> bool {
    size > offset
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{offset_of, size_of};

    // Offsets worked out from ARAInterface.h: packed to 1 byte on x86,
    // natural alignment on ARM.
    #[cfg(target_arch = "x86_64")]
    #[test]
    fn layouts_match_the_header_packed_to_one_byte() {
        assert_eq!(size_of::<MusicalContextProperties>(), 28);
        assert_eq!(offset_of!(MusicalContextProperties, color), 20);
        assert_eq!(size_of::<RegionSequenceProperties>(), 36);
        assert_eq!(offset_of!(RegionSequenceProperties, musicalContextRef), 20);
        assert_eq!(offset_of!(RegionSequenceProperties, color), 28);
        assert_eq!(size_of::<AudioSourceProperties>(), 60);
        assert_eq!(offset_of!(AudioSourceProperties, channelCount), 40);
        assert_eq!(offset_of!(AudioSourceProperties, merits64BitSamples), 44);
        assert_eq!(offset_of!(AudioSourceProperties, channelArrangement), 52);
        assert_eq!(size_of::<PlaybackRegionProperties>(), 76);
        assert_eq!(
            offset_of!(PlaybackRegionProperties, startInModificationTime),
            12
        );
        assert_eq!(offset_of!(PlaybackRegionProperties, musicalContextRef), 44);
        assert_eq!(offset_of!(PlaybackRegionProperties, regionSequenceRef), 52);
        assert_eq!(offset_of!(PlaybackRegionProperties, color), 68);
        assert_eq!(size_of::<ContentNote>(), 44);
        assert_eq!(offset_of!(ContentNote, startPosition), 12);
        assert_eq!(size_of::<ContentKeySignature>(), 32);
        assert_eq!(offset_of!(ContentKeySignature, name), 16);
        assert_eq!(size_of::<ContentChord>(), 36);
        assert_eq!(offset_of!(ContentChord, name), 20);
        assert_eq!(offset_of!(ContentChord, position), 28);
        assert_eq!(size_of::<RestoreObjectsFilter>(), 60);
        assert_eq!(offset_of!(RestoreObjectsFilter, audioSourceIDsCount), 12);
        assert_eq!(size_of::<StoreObjectsFilter>(), 44);
        assert_eq!(offset_of!(StoreObjectsFilter, audioModificationRefs), 36);
        assert_eq!(size_of::<InterfaceConfiguration>(), 20);
        assert_eq!(
            offset_of!(InterfaceConfiguration, assertFunctionAddress),
            12
        );
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn layouts_match_the_header_aligned_naturally() {
        assert_eq!(size_of::<MusicalContextProperties>(), 32);
        assert_eq!(offset_of!(MusicalContextProperties, color), 24);
        assert_eq!(size_of::<AudioSourceProperties>(), 64);
        assert_eq!(offset_of!(AudioSourceProperties, channelArrangement), 56);
        assert_eq!(size_of::<PlaybackRegionProperties>(), 80);
        assert_eq!(
            offset_of!(PlaybackRegionProperties, startInModificationTime),
            16
        );
        assert_eq!(size_of::<ContentNote>(), 48);
        assert_eq!(size_of::<ContentChord>(), 40);
        assert_eq!(size_of::<InterfaceConfiguration>(), 24);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn layouts_the_packing_does_not_change() {
        // All pointer-sized (or 32-bit pairs that keep 8-byte alignment).
        assert_eq!(size_of::<Factory>(), 128);
        assert_eq!(offset_of!(Factory, factoryID), 16);
        assert_eq!(
            offset_of!(Factory, createDocumentControllerWithDocument),
            72
        );
        assert_eq!(
            offset_of!(Factory, supportedPlaybackTransformationFlags),
            120
        );
        assert_eq!(offset_of!(Factory, supportsStoringAudioFileChunks), 124);
        assert_eq!(size_of::<DocumentControllerHostInstance>(), 88);
        assert_eq!(size_of::<AudioAccessControllerInterface>(), 32);
        assert_eq!(size_of::<ArchivingControllerInterface>(), 56);
        assert_eq!(size_of::<ContentAccessControllerInterface>(), 80);
        assert_eq!(size_of::<ModelUpdateControllerInterface>(), 48);
        assert_eq!(size_of::<PlaybackControllerInterface>(), 48);
        // 54 functions after structSize.
        assert_eq!(size_of::<DocumentControllerInterface>(), 8 * 55);
        assert_eq!(
            offset_of!(DocumentControllerInterface, destroyContentReader),
            8 * 41
        );
        assert_eq!(
            offset_of!(DocumentControllerInterface, restoreObjectsFromArchive),
            8 * 46
        );
        assert_eq!(size_of::<PlugInExtensionInstance>(), 72);
        assert_eq!(size_of::<ViewSelection>(), 48);
        assert_eq!(size_of::<ContentTempoEntry>(), 16);
        assert_eq!(size_of::<ContentBarSignature>(), 16);
        assert_eq!(size_of::<ContentTuning>(), 64);
    }
}
