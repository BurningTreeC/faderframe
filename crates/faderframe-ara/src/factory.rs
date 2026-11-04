//! A plug-in's ARA factory.

use crate::content::text;
use crate::{AraError, ffi, host};
use std::ffi::c_void;
use std::mem::offset_of;
use std::sync::Mutex;

/// A plug-in's static ARA factory. It lives as long as the plug-in's
/// binary, which FaderFrame never unloads (VST3 modules and CLAP entries
/// stay loaded for the life of the process).
#[derive(Clone, Copy)]
pub struct Factory {
    /// Read with `read_unaligned`: a plug-in's struct is packed and may sit
    /// at any address.
    raw: *const ffi::Factory,
}

impl std::fmt::Debug for Factory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Factory")
            .field("id", &self.id())
            .field("name", &self.name())
            .finish()
    }
}

impl PartialEq for Factory {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self.raw, other.raw)
    }
}

/// The factories initialised in this process (by address), with the API
/// generation each runs at.
static INITIALISED: Mutex<Vec<(usize, ffi::APIGeneration)>> = Mutex::new(Vec::new());

impl Factory {
    /// The factory at `ptr`.
    ///
    /// # Safety
    /// `ptr` is null or points to an `ARAFactory` that stays valid (and
    /// unchanged) for the rest of the process: the plug-in's binary is
    /// never unloaded.
    pub unsafe fn from_raw(ptr: *const c_void) -> Option<Factory> {
        if ptr.is_null() {
            return None;
        }
        let raw = ptr as *const ffi::Factory;
        // SAFETY: the caller's contract (copied: any alignment).
        let f = unsafe { raw.read_unaligned() };
        let min = offset_of!(ffi::Factory, supportedPlaybackTransformationFlags)
            + std::mem::size_of::<ffi::PlaybackTransformationFlags>();
        if f.structSize < min
            || f.factoryID.is_null()
            || f.initializeARAWithConfiguration.is_none()
            || f.createDocumentControllerWithDocument.is_none()
        {
            return None;
        }
        Some(Factory { raw })
    }

    /// The factory's fields (a copy; the factory never changes).
    pub(crate) fn raw(&self) -> ffi::Factory {
        // SAFETY: valid for the life of the process (`from_raw`'s contract).
        let mut f = unsafe { self.raw.read_unaligned() };
        if !ffi::implements(
            f.structSize,
            offset_of!(ffi::Factory, supportsStoringAudioFileChunks),
        ) {
            f.supportsStoringAudioFileChunks = ffi::FALSE;
        }
        f
    }

    /// The address it was made from (what the companion API handed out).
    pub fn as_ptr(&self) -> *const std::ffi::c_void {
        self.raw as *const std::ffi::c_void
    }

    /// Globally unique, versioned identifier of the plug-in's document
    /// controller class (the same through every companion API).
    pub fn id(&self) -> String {
        // SAFETY: checked non-null in `from_raw`; the factory's strings
        // live as long as it does.
        unsafe { text(self.raw().factoryID) }.unwrap_or_default()
    }

    pub fn name(&self) -> String {
        // SAFETY: as above (null allowed).
        unsafe { text(self.raw().plugInName) }.unwrap_or_default()
    }

    pub fn vendor(&self) -> String {
        // SAFETY: as above.
        unsafe { text(self.raw().manufacturerName) }.unwrap_or_default()
    }

    pub fn version(&self) -> String {
        // SAFETY: as above.
        unsafe { text(self.raw().version) }.unwrap_or_default()
    }

    pub fn information_url(&self) -> String {
        // SAFETY: as above.
        unsafe { text(self.raw().informationURL) }.unwrap_or_default()
    }

    /// The identifier of the archives its document controllers write.
    pub fn archive_id(&self) -> String {
        // SAFETY: as above.
        unsafe { text(self.raw().documentArchiveID) }.unwrap_or_default()
    }

    /// Older archive identifiers it can read, preferred first.
    pub fn compatible_archive_ids(&self) -> Vec<String> {
        let f = self.raw();
        let n = f.compatibleDocumentArchiveIDsCount.min(1024);
        let ids = f.compatibleDocumentArchiveIDs;
        if ids.is_null() {
            return Vec::new();
        }
        (0..n)
            // SAFETY: an array of `n` strings the factory owns.
            .filter_map(|i| unsafe { text(ids.add(i).read_unaligned()) })
            .collect()
    }

    /// Whether it can restore an archive written under `id`.
    pub fn reads_archive(&self, id: &str) -> bool {
        self.archive_id() == id || self.compatible_archive_ids().iter().any(|c| c == id)
    }

    /// The playback transformations it supports (`kARAPlaybackTransformation…`).
    pub fn transformations(&self) -> ffi::PlaybackTransformationFlags {
        self.raw().supportedPlaybackTransformationFlags
    }

    /// Whether it time-stretches playback regions.
    pub fn stretches(&self) -> bool {
        self.transformations() & ffi::kARAPlaybackTransformationTimestretch != 0
    }

    /// The content types it can analyse without its UI.
    pub fn analyzable(&self) -> Vec<ffi::ContentType> {
        let f = self.raw();
        let n = f.analyzeableContentTypesCount.min(64);
        let types = f.analyzeableContentTypes;
        if types.is_null() {
            return Vec::new();
        }
        // SAFETY: an array of `n` content types the factory owns.
        (0..n)
            .map(|i| unsafe { types.add(i).read_unaligned() })
            .collect()
    }

    /// Initialise ARA for this factory (once per process; later calls
    /// return the generation chosen the first time). FaderFrame needs ARA
    /// 2.0 Final (region sequences, partial persistency) and asks for the
    /// newest generation both sides know, up to 2.3.
    pub(crate) fn initialize(&self) -> Result<ffi::APIGeneration, AraError> {
        let address = self.raw as usize;
        let mut done = INITIALISED
            .lock()
            .map_err(|_| AraError::Other("ARA initialisation lock poisoned".into()))?;
        if let Some(&(_, generation)) = done.iter().find(|(a, _)| *a == address) {
            return Ok(generation);
        }
        let raw = self.raw();
        let lowest = raw.lowestSupportedApiGeneration;
        let highest = raw.highestSupportedApiGeneration;
        let generation = highest.min(ffi::kARAAPIGeneration_2_3_Final);
        if generation < ffi::kARAAPIGeneration_2_0_Final || generation < lowest {
            return Err(AraError::Generation {
                plugin: self.name(),
                lowest,
                highest,
            });
        }
        let config = ffi::InterfaceConfiguration {
            structSize: std::mem::size_of::<ffi::InterfaceConfiguration>(),
            desiredApiGeneration: generation,
            assertFunctionAddress: &host::ASSERT_FUNCTION,
        };
        let init = raw
            .initializeARAWithConfiguration
            .ok_or_else(|| AraError::InvalidFactory(self.name()))?;
        // SAFETY: called once per factory, on the control thread, with a
        // configuration valid for the call; the assert function's address
        // is a static (valid until ARA is uninitialised, i.e. for ever).
        unsafe { init(&config) };
        done.push((address, generation));
        tracing::info!(
            "ARA: {} {} ({}) initialised at API generation {generation}",
            self.name(),
            self.version(),
            self.id()
        );
        Ok(generation)
    }
}
