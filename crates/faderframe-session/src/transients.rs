//! Transients of audio sources (for Tab to Transient, display, separating
//! and warping).
//!
//! Detection ([`faderframe_audio_files::onsets`]) runs in a background
//! thread per source, reading the media file (or the in-memory data of
//! generated sources); results are cached next to the media as
//! `<name>.fftr` (JSON) so they are computed once. Every onset keeps its
//! strength; the editor's sensitivity filters at use time.

use crate::Session;
use faderframe_audio_files::onsets::{self, Onset};
use faderframe_core::AudioSourceId;
use faderframe_engine::Source;
use faderframe_project::{Clip, ClipContent};
use faderframe_timeline::MusicalTime;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::JoinHandle;

/// Detected onsets per source, and the analyses running.
#[derive(Default)]
pub(crate) struct TransientCache {
    pub(crate) onsets: HashMap<AudioSourceId, Arc<Vec<Onset>>>,
    jobs: Vec<(AudioSourceId, JoinHandle<Vec<Onset>>)>,
}

fn cache_path(media: &Path) -> PathBuf {
    media.with_extension("fftr")
}

fn load(media: &Path) -> Option<Vec<Onset>> {
    let text = std::fs::read_to_string(cache_path(media)).ok()?;
    // Stale when the media is newer than the cache.
    let m = std::fs::metadata(media).ok()?.modified().ok()?;
    let c = std::fs::metadata(cache_path(media)).ok()?.modified().ok()?;
    if c < m {
        return None;
    }
    serde_json::from_str(&text).ok()
}

/// Read a media file and detect its onsets (helper thread).
fn analyse_file(media: &Path) -> std::io::Result<Vec<Onset>> {
    let f = faderframe_audio_files::wavstream::WavFile::open(media)?;
    let chunk = 1 << 16;
    let ch = f.channels();
    let mut mono = Vec::with_capacity(f.frames() as usize);
    let mut bufs = vec![vec![0.0f32; chunk]; ch];
    let mut scratch = Vec::new();
    let mut pos = 0u64;
    while pos < f.frames() {
        let n = chunk.min((f.frames() - pos) as usize);
        {
            let mut slices: Vec<&mut [f32]> = bufs.iter_mut().map(|c| &mut c[..n]).collect();
            f.read(pos, &mut slices, &mut scratch)?;
        }
        let views: Vec<&[f32]> = bufs.iter().map(|c| &c[..n]).collect();
        mono.extend(onsets::mixdown(&views));
        pos += n as u64;
    }
    let found = onsets::detect(&mono, f.sample_rate());
    if let Ok(json) = serde_json::to_string(&found) {
        let _ = std::fs::write(cache_path(media), json);
    }
    Ok(found)
}

impl Session {
    /// Start detecting transients for every source not analysed yet.
    pub(crate) fn analyse_transients_of_project(&mut self) {
        let wanted: Vec<(AudioSourceId, Source)> = self
            .sources
            .iter()
            .filter(|(id, _)| {
                !self.transients.onsets.contains_key(id)
                    && !self.transients.jobs.iter().any(|(j, _)| j == *id)
            })
            .map(|(id, s)| (*id, s.clone()))
            .collect();
        for (id, source) in wanted {
            if let Source::Stream(st) = &source
                && let Some(found) = load(st.path())
            {
                self.transients.onsets.insert(id, Arc::new(found));
                continue;
            }
            let spawned = std::thread::Builder::new()
                .name("faderframe-transients".into())
                .spawn(move || match source {
                    Source::Memory(d) => {
                        let views: Vec<&[f32]> =
                            (0..d.num_channels()).map(|c| d.channel(c)).collect();
                        onsets::detect(&onsets::mixdown(&views), d.sample_rate())
                    }
                    Source::Stream(st) => analyse_file(st.path()).unwrap_or_default(),
                });
            if let Ok(h) = spawned {
                self.transients.jobs.push((id, h));
            }
        }
        self.revision += 1;
    }

    /// Collect finished analyses (from the session tick).
    pub(crate) fn poll_transients(&mut self) {
        let mut i = 0;
        while i < self.transients.jobs.len() {
            if self.transients.jobs[i].1.is_finished() {
                let (id, h) = self.transients.jobs.swap_remove(i);
                if let Ok(found) = h.join() {
                    self.transients.onsets.insert(id, Arc::new(found));
                    self.revision += 1;
                }
            } else {
                i += 1;
            }
        }
    }

    /// Transient analyses still running.
    pub fn analysing_transients(&self) -> bool {
        !self.transients.jobs.is_empty()
    }

    /// Onsets of a source at the editor's sensitivity (source frames), or
    /// `None` while not analysed.
    pub fn source_transients(&self, source: AudioSourceId) -> Option<Vec<i64>> {
        let min = onsets::strength_threshold(self.editor.transient_sensitivity);
        self.transients.onsets.get(&source).map(|o| {
            o.iter()
                .filter(|x| x.strength >= min)
                .map(|x| x.frame)
                .collect()
        })
    }

    /// Transients inside an audio clip, as clip-relative frames.
    pub fn clip_transient_frames(&self, clip: &Clip) -> Vec<i64> {
        let ClipContent::Audio(a) = &clip.content else {
            return Vec::new();
        };
        let Some(frames) = self.source_transients(a.source) else {
            return Vec::new();
        };
        let warp = a.warp.as_ref();
        frames
            .into_iter()
            .filter(|f| match warp {
                Some(w) => w.source_range(a.source_offset).contains(f),
                None => *f >= a.source_offset && *f < a.source_offset + a.length,
            })
            .map(|f| match warp {
                Some(w) => w.output_of(a.source_offset, a.length, f),
                None => f - a.source_offset,
            })
            .collect()
    }

    /// Transients inside a clip on the timeline.
    pub(crate) fn clip_transients(&self, clip: &Clip) -> Vec<MusicalTime> {
        let p = &self.project;
        let rate = p.sample_rate as f64;
        let base = p.timeline.to_samples(clip.start, rate);
        self.clip_transient_frames(clip)
            .into_iter()
            .map(|f| p.timeline.to_musical(base + f, rate))
            .collect()
    }
}
