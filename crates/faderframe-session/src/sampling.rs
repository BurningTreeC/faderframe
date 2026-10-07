//! Samples made from audio: the edit selection of an audio track (or its
//! selected clips) rendered as the clips play it — clip gain, fades, warp
//! and reverse applied, without the track's plugins, fader or pan — into
//! the media folder's `Samples`, then played by a new Sampler (its root key
//! and tuning found in the audio), a new Drum Sampler, the next free pad of
//! a Drum Sampler there is, or saved as a 24-bit WAV file.
//!
//! The render and the work after it (trimming, edge fades, the pitch,
//! decoding for the samplers) run on a worker thread; the session places
//! the result in its tick, as one undo step.

use crate::render::{self, RenderChannels, RenderRange, RenderSettings};
use crate::samples::{self, SAMPLES_FOLDER};
use crate::{Action, NoticeLevel, Result, SelectMode, Session, SessionError, UiRequest};
use faderframe_audio_files::{Dither, WavFormat, read_wav, write_wav_with};
use faderframe_automation::AutomationTarget;
use faderframe_core::{ChannelLayout, ClipId, PluginInstanceId, TrackId, builtin};
use faderframe_plugin_host::devices::samples::{Sample, SampleDoc};
use faderframe_plugin_host::devices::{drums, note_name, sampler};
use faderframe_project::{Command, PluginRef, PluginSlot, Track, TrackColor, TrackKind};
use faderframe_timeline::MusicalTime;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread::JoinHandle;

/// Where a sample goes.
#[derive(Clone, Debug, PartialEq)]
pub enum SampleTarget {
    /// A new instrument track playing it with the Sampler.
    Sampler,
    /// A new instrument track with the Drum Sampler, the sample on pad 1.
    Drums,
    /// The next free pad of this Drum Sampler.
    Pad(PluginInstanceId),
    /// A 24-bit WAV file.
    File(PathBuf),
}

/// A sample being made.
pub(crate) struct PendingSample {
    track: TrackId,
    name: String,
    target: SampleTarget,
    handle: Option<JoinHandle<std::result::Result<Made, String>>>,
}

/// What the worker hands back.
struct Made {
    path: PathBuf,
    /// The note it plays (cents as the fraction), when it has a clear one.
    root: Option<f64>,
    /// Keeps it decoded in the samplers' cache until a sampler has it.
    held: Option<Arc<Sample>>,
}

/// The fade at an end that cuts through sound (seconds): no click, and
/// too short to soften an attack.
const EDGE_FADE: f64 = 0.001;
/// Rendered past the end, to make up for any latency.
const SPARE_SECONDS: f32 = 0.25;

impl Session {
    /// The span of `track` a sample would be made of, and what it is
    /// ("Selection", "Clip", "Clips"): the edit selection when the track is
    /// in it, else the selected clips on the track, else `clip`. Audio
    /// tracks only.
    fn sample_span(
        &self,
        track: TrackId,
        clip: Option<ClipId>,
    ) -> Option<(MusicalTime, MusicalTime, &'static str)> {
        let t = self.project.track(track)?;
        if t.kind != TrackKind::Audio {
            return None;
        }
        if let Some(r) = self.selection.range
            && r.end > r.start
            && self.selection.tracks.contains(&track)
        {
            return Some((r.start, r.end, "Selection"));
        }
        let p = &self.project;
        let mut clips: Vec<_> = self
            .selection
            .clips
            .iter()
            .filter_map(|c| p.clip(*c))
            .filter(|c| c.track == track)
            .collect();
        if clips.is_empty() {
            clips.extend(clip.and_then(|c| p.clip(c)).filter(|c| c.track == track));
        }
        let start = clips.iter().map(|c| c.start).min()?;
        let end = clips
            .iter()
            .map(|c| c.end(&p.timeline, p.sample_rate))
            .max()?;
        let what = if clips.len() > 1 { "Clips" } else { "Clip" };
        (end > start).then_some((start, end, what))
    }

    /// Menu entries for making a sample of `track` (the clip under the
    /// pointer when there is no selection): label, action, and whether a
    /// separator goes before it. Empty when there is nothing to sample.
    pub fn sample_choices(
        &self,
        track: TrackId,
        clip: Option<ClipId>,
    ) -> Vec<(String, Action, bool)> {
        let Some((start, end, what)) = self.sample_span(track, clip) else {
            return Vec::new();
        };
        let make = |target| Action::MakeSample {
            track,
            start,
            end,
            target,
        };
        let mut out = vec![
            (
                format!("{what} to New Sampler"),
                make(SampleTarget::Sampler),
                true,
            ),
            (
                format!("{what} to New Drum Sampler"),
                make(SampleTarget::Drums),
                false,
            ),
        ];
        for t in &self.project.tracks {
            let kits = t
                .instrument
                .iter()
                .chain(&t.inserts)
                .filter(|s| s.plugin.id == builtin::DRUMS);
            for kit in kits {
                out.push((
                    format!("{what} to the Next Free Pad of ‘{}’", t.name),
                    make(SampleTarget::Pad(kit.id)),
                    false,
                ));
            }
        }
        out.push((
            format!("Save {what} as Sample…"),
            Action::PromptSaveSample { track, start, end },
            false,
        ));
        out
    }

    /// Samples being made of these tracks.
    pub fn sampling(&self) -> Vec<TrackId> {
        self.samplings.iter().map(|s| s.track).collect()
    }

    /// Ask where to save a sample (the shell then sends `MakeSample`).
    pub(crate) fn prompt_save_sample(
        &mut self,
        track: TrackId,
        start: MusicalTime,
        end: MusicalTime,
    ) -> Result<()> {
        let t = self
            .project
            .track(track)
            .ok_or_else(|| SessionError::Other(format!("no track {track}")))?;
        let name = format!("{} Sample.wav", t.name);
        self.ui_requests.push(UiRequest::SaveSample {
            track,
            start,
            end,
            name,
        });
        Ok(())
    }

    /// Render `track` from `start` to `end` in the background, then give
    /// it to `target`.
    pub(crate) fn make_sample(
        &mut self,
        track: TrackId,
        start: MusicalTime,
        end: MusicalTime,
        target: SampleTarget,
    ) -> Result<()> {
        let t = self
            .project
            .track(track)
            .ok_or_else(|| SessionError::Other(format!("no track {track}")))?;
        if t.kind != TrackKind::Audio {
            return Err(SessionError::Other(
                "samples are made of audio tracks".into(),
            ));
        }
        if end <= start {
            return Err(SessionError::Other("select a range to sample".into()));
        }
        let source = t.name.clone();
        let name = format!("{source} Sample");
        let mono = t.layout == ChannelLayout::Mono;
        let project = self.absolute_copy();
        let (mut copy, _, _) = render::track_render_project(&project, track)
            .ok_or_else(|| SessionError::Other(format!("'{source}' has no clips")))?;
        // The clips as they play: no plugins on the track.
        if let Some(t) = copy.track_mut(track) {
            t.inserts.clear();
            t.preamp = None;
            t.instrument = None;
            t.automation.lanes.retain(|l| {
                !matches!(
                    l.target,
                    AutomationTarget::PluginParameter { .. } | AutomationTarget::PluginBypass(_)
                )
            });
        }
        // Without its devices a mono track is mono to the end (the render
        // project kept a stereo master for one a device widened).
        if mono && let Some(m) = copy.master_id().and_then(|m| copy.track_mut(m)) {
            m.layout = ChannelLayout::Mono;
        }
        let rate = project.sample_rate;
        let frames = (project.timeline.to_samples(end, f64::from(rate))
            - project.timeline.to_samples(start, f64::from(rate)))
        .max(0) as usize;
        let dir = self.media_dir.join(SAMPLES_FOLDER);
        std::fs::create_dir_all(&dir)
            .map_err(|e| SessionError::Other(format!("{}: {e}", dir.display())))?;
        let scratch = crate::media::unique_path(&dir, &format!("{name} (rendering)"));
        let file = match &target {
            SampleTarget::File(path) => path.clone(),
            _ => {
                let path = crate::media::unique_path(&dir, &name);
                // Taken now, so a second sample made meanwhile gets its own.
                std::fs::File::create(&path)
                    .map_err(|e| SessionError::Other(format!("{}: {e}", path.display())))?;
                path
            }
        };
        let settings = RenderSettings {
            range: RenderRange::Span { start, end },
            channels: if mono {
                RenderChannels::First
            } else {
                RenderChannels::Stereo
            },
            sample_rate: rate,
            tail_seconds: SPARE_SECONDS,
            normalize_db: None,
            format: WavFormat::Float32,
            dither: Dither::Off,
            report: false,
            ..RenderSettings::defaults_for(&copy, scratch.clone())
        };
        let job = render::start(copy, settings).map_err(|e| SessionError::Other(e.to_string()))?;
        let want_root = target == SampleTarget::Sampler;
        let to_file = matches!(target, SampleTarget::File(_));
        let handle = std::thread::Builder::new()
            .name("faderframe-sample".into())
            .spawn(move || {
                let rendered = job.join();
                let wav = rendered
                    .map_err(|e| e.to_string())
                    .and_then(|_| read_wav(&scratch).map_err(|e| e.to_string()));
                let _ = std::fs::remove_file(&scratch);
                let wav = wav?;
                let mut channels = wav.channels;
                trim(&mut channels, frames, wav.sample_rate);
                let root = if want_root {
                    let n = channels.first().map_or(0, Vec::len);
                    let mix: Vec<f32> = (0..n)
                        .map(|i| channels.iter().map(|c| c[i]).sum::<f32>() / channels.len() as f32)
                        .collect();
                    faderframe_analysis::pitch::root_key(&mix, f64::from(wav.sample_rate))
                } else {
                    None
                };
                let (format, dither) = if to_file {
                    (WavFormat::Pcm24, Dither::Tpdf)
                } else {
                    (WavFormat::Float32, Dither::Off)
                };
                write_wav_with(&file, &channels, wav.sample_rate, format, dither)
                    .map_err(|e| format!("{}: {e}", file.display()))?;
                let held = if to_file {
                    None
                } else {
                    Some(faderframe_plugin_host::devices::samples::load_cached(
                        &file,
                    )?)
                };
                Ok(Made {
                    path: file,
                    root,
                    held,
                })
            })
            .map_err(|e| SessionError::Other(e.to_string()))?;
        self.samplings.push(PendingSample {
            track,
            name,
            target,
            handle: Some(handle),
        });
        self.notify(NoticeLevel::Info, "making a sample…".to_string());
        self.revision += 1;
        Ok(())
    }

    /// Place samples that are done (from the tick).
    pub(crate) fn poll_samples(&mut self) {
        let mut i = 0;
        while i < self.samplings.len() {
            if !self.samplings[i]
                .handle
                .as_ref()
                .is_none_or(JoinHandle::is_finished)
            {
                i += 1;
                continue;
            }
            let mut p = self.samplings.remove(i);
            let made = p
                .handle
                .take()
                .and_then(|h| h.join().ok())
                .unwrap_or_else(|| Err("the sample's worker stopped".into()));
            if let Err(e) = made
                .map_err(SessionError::Other)
                .and_then(|m| self.place_sample(&p, m))
            {
                self.notify(NoticeLevel::Error, format!("sample: {e}"));
            }
            self.revision += 1;
        }
    }

    fn place_sample(&mut self, p: &PendingSample, made: Made) -> Result<()> {
        let file = made.path.to_string_lossy().into_owned();
        match &p.target {
            SampleTarget::File(path) => {
                self.notify(NoticeLevel::Info, format!("saved {}", path.display()));
            }
            SampleTarget::Sampler | SampleTarget::Drums => {
                let is_sampler = p.target == SampleTarget::Sampler;
                // Root key and tuning: the found note plays in tune.
                let mut params = Vec::new();
                let mut root_note = None;
                if is_sampler && let Some(note) = made.root {
                    let root = note.round().clamp(0.0, 127.0);
                    let cents = ((root - note) * 100.0).clamp(-100.0, 100.0);
                    for (id, v) in [(sampler::id::ROOT, root), (sampler::id::TUNE, cents)] {
                        params.extend_from_slice(&id.to_le_bytes());
                        params.extend_from_slice(&v.to_le_bytes());
                    }
                    root_note = Some((root, -cents));
                }
                let doc = SampleDoc {
                    files: vec![Some(file)],
                };
                let state = faderframe_engine::encode_state(
                    &faderframe_plugin_host::devices::samples::pack(&params, &doc),
                );
                let source = self.project.track(p.track).map(|t| t.name.clone());
                let project = &mut self.project;
                let id: TrackId = project.ids.allocate();
                let plugin: PluginInstanceId = project.ids.allocate();
                let index = project
                    .track_index(p.track)
                    .map_or(project.tracks.len(), |i| i + 1);
                let name = if is_sampler {
                    p.name.clone()
                } else {
                    format!("{} Drums", source.as_deref().unwrap_or("Sample"))
                };
                let track = Track::new(
                    id,
                    TrackKind::Instrument,
                    name.clone(),
                    TrackColor::palette(project.tracks.len()),
                );
                let (plugin_id, plugin_name) = if is_sampler {
                    (builtin::SAMPLER, "Sampler")
                } else {
                    (builtin::DRUMS, "Drum Sampler")
                };
                self.batch(
                    if is_sampler {
                        "Sample to Sampler"
                    } else {
                        "Sample to Drum Sampler"
                    },
                    vec![
                        Command::AddTrack {
                            track: Box::new(track),
                            index,
                        },
                        Command::InsertPlugin {
                            track: id,
                            index: 0,
                            slot: PluginSlot {
                                id: plugin,
                                plugin: PluginRef::builtin(plugin_id, plugin_name),
                                bypass: false,
                                parameters: Vec::new(),
                                state: Some(state),
                                sidechain: None,
                            },
                        },
                    ],
                )?;
                drop(made.held);
                self.selection.select_tracks(&[id], SelectMode::Replace);
                self.ui_requests.push(UiRequest::PluginEditor {
                    track: id,
                    plugin,
                    generic: false,
                });
                let how = match root_note {
                    Some((root, cents)) if cents.abs() >= 1.0 => format!(
                        ", root key {} ({:+.0} cents)",
                        note_name(root as i32),
                        cents
                    )
                    .replace('-', "−"),
                    Some((root, _)) => format!(", root key {}", note_name(root as i32)),
                    None if is_sampler => ", root key C4 (no clear pitch found)".into(),
                    None => " on pad 1".into(),
                };
                self.notify(NoticeLevel::Info, format!("'{name}' plays the sample{how}"));
            }
            SampleTarget::Pad(plugin) => {
                let Some((track, owner)) = self.plugin_owner(*plugin) else {
                    return Err(SessionError::Other("the Drum Sampler is gone".into()));
                };
                let kit = self
                    .project
                    .track(track)
                    .map(|t| t.name.clone())
                    .unwrap_or_default();
                let mut doc = samples::doc_of(owner);
                let Some(pad) =
                    (0..drums::PADS).find(|&i| doc.files.get(i).is_none_or(Option::is_none))
                else {
                    return Err(SessionError::Other(format!(
                        "all {} pads of '{kit}' are in use",
                        drums::PADS
                    )));
                };
                doc.set(pad, Some(file));
                self.apply_samples(*plugin, &doc)?;
                drop(made.held);
                self.notify(
                    NoticeLevel::Info,
                    format!("the sample is on pad {} of '{kit}'", pad + 1),
                );
            }
        }
        Ok(())
    }
}

/// Keep `frames`, and fade an end that cuts through sound.
fn trim(channels: &mut [Vec<f32>], frames: usize, rate: u32) {
    for c in channels.iter_mut() {
        c.truncate(frames);
    }
    let len = channels.first().map_or(0, Vec::len);
    let n = ((f64::from(rate) * EDGE_FADE) as usize).max(1);
    if len < 4 * n {
        return;
    }
    let loud = |i: usize| channels.iter().any(|c| c[i].abs() > 1e-3);
    let (head, tail) = (loud(0), loud(len - 1));
    for c in channels.iter_mut() {
        for i in 0..n {
            let g = i as f32 / n as f32;
            if head {
                c[i] *= g;
            }
            if tail {
                c[len - 1 - i] *= g;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ends_that_cut_through_sound_are_faded() {
        let mut ch = vec![vec![0.5f32; 1000], vec![0.0f32; 1000]];
        ch[0][999] = 0.0;
        ch[0][998] = 0.0;
        trim(&mut ch, 900, 48_000);
        assert_eq!(ch[0].len(), 900);
        assert_eq!(ch[0][0], 0.0, "faded in");
        assert!((ch[0][47] - 0.5 * 47.0 / 48.0).abs() < 1e-6);
        assert_eq!(ch[0][899], 0.0, "faded out");
        // Quiet ends stay as they are.
        let mut quiet = vec![vec![0.0f32; 400]];
        quiet[0][200] = 1.0;
        trim(&mut quiet, 400, 48_000);
        assert_eq!(quiet[0][200], 1.0);
    }
}
