//! What binding a companion plug-in instance to a document controller
//! hands back: its playback renderer, editor renderer and editor view.

use crate::ffi;
use std::ffi::c_void;
use std::mem::offset_of;

/// The roles a bound instance takes on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Roles(pub ffi::PlugInInstanceRoleFlags);

impl Roles {
    /// Renders its playback regions for playback and exports.
    pub const PLAYBACK_RENDERER: Roles = Roles(ffi::kARAPlaybackRendererRole);
    /// Adds previews while editing (a dragged note's pitch).
    pub const EDITOR_RENDERER: Roles = Roles(ffi::kARAEditorRendererRole);
    /// Shows the editor (all regions of the document, the host's selection).
    pub const EDITOR_VIEW: Roles = Roles(ffi::kARAEditorViewRole);
    /// FaderFrame's renderer on a track: all three.
    pub const ALL: Roles = Roles(
        ffi::kARAPlaybackRendererRole | ffi::kARAEditorRendererRole | ffi::kARAEditorViewRole,
    );

    pub fn bits(self) -> ffi::PlugInInstanceRoleFlags {
        self.0
    }
}

/// A bound instance's ARA interfaces.
///
/// Valid while the companion instance lives: whoever makes one
/// ([`Extension::from_raw`]) drops it (and every [`crate::Renderer`]
/// holding it) before the instance goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Extension {
    raw: *const ffi::PlugInExtensionInstance,
}

impl Extension {
    /// The extension a binding call returned.
    ///
    /// # Safety
    /// `ptr` is null or what the companion API's binding call returned for
    /// a live instance, and the result is not used after that instance is
    /// destroyed.
    pub unsafe fn from_raw(ptr: *const c_void) -> Option<Extension> {
        if ptr.is_null() {
            return None;
        }
        let raw = ptr as *const ffi::PlugInExtensionInstance;
        // SAFETY: the caller's contract (packed struct: any alignment).
        let size = unsafe { (*raw).structSize };
        // ARA 2 roles start after ARA 1's two fields.
        if size < offset_of!(ffi::PlugInExtensionInstance, playbackRendererRef) {
            return None;
        }
        Some(Extension { raw })
    }

    fn get(&self) -> ffi::PlugInExtensionInstance {
        // SAFETY: valid by the constructor's contract; copied whole, with
        // the fields past its size zeroed below.
        let mut e = unsafe { self.raw.read_unaligned() };
        let size = e.structSize;
        if !ffi::implements(
            size,
            offset_of!(ffi::PlugInExtensionInstance, editorViewRef),
        ) {
            e.editorViewRef = std::ptr::null_mut();
            e.editorViewInterface = std::ptr::null();
        }
        if !ffi::implements(
            size,
            offset_of!(ffi::PlugInExtensionInstance, editorRendererRef),
        ) {
            e.editorRendererRef = std::ptr::null_mut();
            e.editorRendererInterface = std::ptr::null();
        }
        e
    }

    fn playback(&self) -> Option<(ffi::PlaybackRendererRef, ffi::PlaybackRendererInterface)> {
        let e = self.get();
        let i = e.playbackRendererInterface;
        // SAFETY: the plug-in's interface table, valid with the instance.
        (!i.is_null()).then(|| (e.playbackRendererRef, unsafe { i.read_unaligned() }))
    }

    fn editor(&self) -> Option<(ffi::EditorRendererRef, ffi::EditorRendererInterface)> {
        let e = self.get();
        let i = e.editorRendererInterface;
        // SAFETY: as above.
        (!i.is_null()).then(|| (e.editorRendererRef, unsafe { i.read_unaligned() }))
    }

    fn view(&self) -> Option<(ffi::EditorViewRef, ffi::EditorViewInterface)> {
        let e = self.get();
        let i = e.editorViewInterface;
        // SAFETY: as above.
        (!i.is_null()).then(|| (e.editorViewRef, unsafe { i.read_unaligned() }))
    }

    /// It renders playback regions (it has the playback renderer role).
    pub fn plays(&self) -> bool {
        self.playback().is_some()
    }

    /// It previews edits (editor renderer role).
    pub fn previews(&self) -> bool {
        self.editor().is_some()
    }

    /// It shows the editor (editor view role).
    pub fn views(&self) -> bool {
        self.view().is_some()
    }

    /// Only while the companion instance is not active (ARA's rule for
    /// playback renderers).
    pub(crate) fn add_playback_region(&self, region: ffi::PlaybackRegionRef) {
        if let Some((r, i)) = self.playback()
            && let Some(f) = i.addPlaybackRegion
        {
            // SAFETY: a live renderer and a region of its document.
            unsafe { f(r, region) };
        }
    }

    pub(crate) fn remove_playback_region(&self, region: ffi::PlaybackRegionRef) {
        if let Some((r, i)) = self.playback()
            && let Some(f) = i.removePlaybackRegion
        {
            // SAFETY: as above.
            unsafe { f(r, region) };
        }
    }

    pub(crate) fn add_sequence(&self, sequence: ffi::RegionSequenceRef) {
        if let Some((r, i)) = self.editor()
            && let Some(f) = i.addRegionSequence
        {
            // SAFETY: a live editor renderer and a sequence of its document.
            unsafe { f(r, sequence) };
        }
    }

    pub(crate) fn remove_sequence(&self, sequence: ffi::RegionSequenceRef) {
        if let Some((r, i)) = self.editor()
            && let Some(f) = i.removeRegionSequence
        {
            // SAFETY: as above.
            unsafe { f(r, sequence) };
        }
    }

    pub(crate) fn notify_selection(
        &self,
        regions: &[ffi::PlaybackRegionRef],
        sequences: &[ffi::RegionSequenceRef],
        range: Option<(f64, f64)>,
    ) {
        let Some((r, i)) = self.view() else { return };
        let Some(f) = i.notifySelection else { return };
        let range = range.map(|(start, duration)| ffi::ContentTimeRange { start, duration });
        let selection = ffi::ViewSelection {
            structSize: std::mem::size_of::<ffi::ViewSelection>(),
            playbackRegionRefsCount: regions.len(),
            playbackRegionRefs: if regions.is_empty() {
                std::ptr::null()
            } else {
                regions.as_ptr()
            },
            regionSequenceRefsCount: sequences.len(),
            regionSequenceRefs: if sequences.is_empty() {
                std::ptr::null()
            } else {
                sequences.as_ptr()
            },
            timeRange: range
                .as_ref()
                .map_or(std::ptr::null(), |r| r as *const ffi::ContentTimeRange),
        };
        // SAFETY: a live editor view; the selection is valid for the call.
        unsafe { f(r, &selection) };
    }
}
