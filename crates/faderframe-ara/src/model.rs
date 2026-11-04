//! The model graph a host wants the plug-in to have, by key.
//!
//! Keys are the host's names for its objects: for audio sources and audio
//! modifications they are also the persistent IDs archives refer to (seven
//! bit ASCII, unique within the document, stable across saves). A region
//! never changes its modification under the same key (put the
//! modification in the region's key).

use crate::{Context, Extension, Samples, ffi};
use std::collections::BTreeSet;
use std::sync::Arc;

/// Everything the document should hold.
#[derive(Clone, Default)]
pub struct Model {
    /// The document's name (the project's).
    pub name: Option<String>,
    /// The one musical context: tempo, bars, keys, chords.
    pub context: Context,
    pub sequences: Vec<Sequence>,
    pub sources: Vec<Source>,
    pub modifications: Vec<Modification>,
    pub regions: Vec<Region>,
}

impl Model {
    pub fn source(&self, key: &str) -> Option<&Source> {
        self.sources.iter().find(|s| s.key == key)
    }

    pub fn region(&self, key: &str) -> Option<&Region> {
        self.regions.iter().find(|r| r.key == key)
    }

    /// The keys of the regions on a sequence.
    pub fn regions_of(&self, sequence: &str) -> BTreeSet<String> {
        self.regions
            .iter()
            .filter(|r| r.sequence == sequence)
            .map(|r| r.key.clone())
            .collect()
    }

    /// Whether every reference resolves (regions to modifications and
    /// sequences, modifications to sources) and keys are unique.
    pub fn is_consistent(&self) -> bool {
        let unique = |keys: Vec<&str>| {
            let n = keys.len();
            keys.into_iter().collect::<BTreeSet<_>>().len() == n
        };
        unique(self.sequences.iter().map(|s| s.key.as_str()).collect())
            && unique(self.sources.iter().map(|s| s.key.as_str()).collect())
            && unique(self.modifications.iter().map(|s| s.key.as_str()).collect())
            && unique(self.regions.iter().map(|s| s.key.as_str()).collect())
            && self
                .modifications
                .iter()
                .all(|m| self.source(&m.source).is_some())
            && self.regions.iter().all(|r| {
                self.modifications.iter().any(|m| m.key == r.modification)
                    && self.sequences.iter().any(|s| s.key == r.sequence)
            })
    }
}

/// A region sequence: one track's regions, played through one routing.
#[derive(Clone, Debug, PartialEq)]
pub struct Sequence {
    pub key: String,
    pub name: Option<String>,
    /// The track's place (strictly increasing in the host's order).
    pub order: i32,
    pub color: Option<[f32; 3]>,
}

/// An audio source: one media file.
#[derive(Clone)]
pub struct Source {
    /// Also its persistent ID.
    pub key: String,
    pub name: Option<String>,
    pub frames: i64,
    pub sample_rate: f64,
    pub channels: u32,
    /// Where readers get its samples. Replaced in place when the file
    /// moves (the plug-in keeps its edits).
    pub samples: Arc<dyn Samples>,
}

impl Source {
    pub(crate) fn same_shape(&self, other: &Source) -> bool {
        self.frames == other.frames
            && self.sample_rate == other.sample_rate
            && self.channels == other.channels
            && self.name == other.name
    }
}

/// An audio modification: the plug-in's edits of a source.
#[derive(Clone, Debug, PartialEq)]
pub struct Modification {
    /// Also its persistent ID.
    pub key: String,
    pub source: String,
    pub name: Option<String>,
}

/// A playback region: a stretch of a modification placed in song time.
#[derive(Clone, Debug, PartialEq)]
pub struct Region {
    pub key: String,
    pub modification: String,
    pub sequence: String,
    /// Seconds of the modification (source time).
    pub start_in_modification: f64,
    pub duration_in_modification: f64,
    /// Seconds of song time.
    pub start_in_playback: f64,
    pub duration_in_playback: f64,
    /// `kARAPlaybackTransformation…` (only what the factory supports).
    pub transformation: ffi::PlaybackTransformationFlags,
    pub name: Option<String>,
    pub color: Option<[f32; 3]>,
}

/// A bound instance with the regions it renders and the sequences it
/// previews (its playback and editor renderer roles).
#[derive(Debug)]
pub struct Renderer {
    extension: Extension,
    pub(crate) regions: BTreeSet<String>,
    pub(crate) sequences: BTreeSet<String>,
}

impl Renderer {
    pub fn new(extension: Extension) -> Self {
        Self {
            extension,
            regions: BTreeSet::new(),
            sequences: BTreeSet::new(),
        }
    }

    pub fn extension(&self) -> &Extension {
        &self.extension
    }

    /// The regions it renders now.
    pub fn regions(&self) -> &BTreeSet<String> {
        &self.regions
    }

    /// Whether giving it `regions` changes its playback renderer's set
    /// (which ARA allows only while the instance is inactive).
    pub fn changes(&self, regions: &BTreeSet<String>) -> bool {
        self.extension.plays() && self.regions != *regions
    }
}

/// What one renderer should render after a sync.
pub struct Assignment<'a> {
    pub renderer: &'a mut Renderer,
    /// Playback regions (needs the instance inactive when they differ from
    /// what it has: [`Renderer::changes`]).
    pub regions: BTreeSet<String>,
    /// Region sequences it previews.
    pub sequences: BTreeSet<String>,
}
