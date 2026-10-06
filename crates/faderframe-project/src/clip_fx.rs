//! Clip effects: a chain of devices on one audio clip, rendered offline.
//!
//! The clip plays the rendered audio; [`ClipEffects`] keeps the chain and
//! the clip's audio as it was before (its source, region, warp, pitch
//! edit and reversal: what the chain processes), so the chain can change
//! and render again, or go and give the clip its audio back. The rendered
//! file starts with the devices' latency: `rendered_offset` is the source
//! frame in it where the original region begins, so trims made since
//! carry over to the next render.

use crate::pitch::PitchEdit;
use crate::{AudioClip, PluginSlot, StretchSettings, Warp};
use faderframe_core::AudioSourceId;
use serde::{Deserialize, Serialize};

/// The audio a chain processes: a clip's as it was.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OriginalAudio {
    pub source: AudioSourceId,
    pub source_offset: i64,
    pub length: i64,
    #[serde(default)]
    pub stretch: StretchSettings,
    #[serde(default)]
    pub reversed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warp: Option<Warp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pitch: Option<PitchEdit>,
}

impl OriginalAudio {
    pub fn of(a: &AudioClip) -> Self {
        Self {
            source: a.source,
            source_offset: a.source_offset,
            length: a.length,
            stretch: a.stretch,
            reversed: a.reversed,
            warp: a.warp.clone(),
            pitch: a.pitch.clone(),
        }
    }

    /// `a` playing this audio again; `trim` frames moved in at the start
    /// and its length as it is now (a trimmed warped clip gets its whole
    /// original region back).
    pub fn restore(&self, a: &mut AudioClip, trim: i64) {
        a.source = self.source;
        a.stretch = self.stretch;
        a.reversed = self.reversed;
        a.warp = self.warp.clone();
        a.pitch = self.pitch.clone();
        if self.warp.is_some() || self.reversed {
            a.source_offset = self.source_offset;
            a.length = self.length;
        } else {
            a.source_offset = self.source_offset + trim.max(0);
            a.length = a.length.min(self.length - trim.max(0)).max(1);
        }
        a.effects = None;
    }
}

/// A clip's effects (see the module docs).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClipEffects {
    pub chain: Vec<PluginSlot>,
    pub original: OriginalAudio,
    /// Where the original region begins in the rendered source.
    pub rendered_offset: i64,
}

impl ClipEffects {
    /// Frames the clip's start has moved in since the render.
    pub fn trim(&self, a: &AudioClip) -> i64 {
        a.source_offset - self.rendered_offset
    }
}
