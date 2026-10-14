//! Merge Clips: the selected clips of a track become one. MIDI clips join
//! at once (notes, controllers and SysEx placed from the first clip's
//! start; notes get fresh ids). Audio clips are rendered as they play —
//! gain, fades, warps, takes, gaps as silence, without the track's
//! devices — in a worker, into a new file in the project's media; when it
//! is done the clips give way to one clip playing it, as one "Merge Clips"
//! step.

use crate::render::{self, RenderChannels, RenderJob, RenderRange, RenderSettings};
use crate::{NoticeLevel, Result, SelectMode, Session, SessionError};
use faderframe_audio_files::WavFormat;
use faderframe_core::{ChannelLayout, ClipId, NoteId, TrackId};
use faderframe_project::{
    AudioClip, AudioSource, Clip, ClipContent, ClipFades, Command, MidiClip, Project, SourceSpec,
    StretchSettings, Track, TrackColor, TrackKind,
};
use faderframe_timeline::MusicalTime;
use std::collections::HashMap;
use std::path::PathBuf;

/// An audio merge being rendered.
pub(crate) struct MergeJob {
    track: TrackId,
    clips: Vec<ClipId>,
    start: MusicalTime,
    name: String,
    job: RenderJob,
    path: PathBuf,
}

impl Session {
    /// Merge the clips: per track, its selected clips into one (two or
    /// more of them).
    pub(crate) fn merge_clips(&mut self, clips: &[ClipId]) -> Result<()> {
        let mut by_track: Vec<(TrackId, Vec<Clip>)> = Vec::new();
        for id in clips {
            let Some(c) = self.project.clip(*id).cloned() else {
                continue;
            };
            match by_track.iter_mut().find(|(t, _)| *t == c.track) {
                Some((_, v)) => v.push(c),
                None => by_track.push((c.track, vec![c])),
            }
        }
        by_track.retain(|(_, v)| v.len() >= 2);
        if by_track.is_empty() {
            return Err(SessionError::Other(
                "select two or more clips of one track to merge".into(),
            ));
        }
        let mut midi_commands = Vec::new();
        let mut merged = Vec::new();
        for (track, mut group) in by_track {
            group.sort_by_key(|c| c.start);
            if group.iter().all(|c| c.as_midi().is_some()) {
                let (commands, id) = self.merge_midi(track, &group);
                midi_commands.extend(commands);
                merged.push(id);
            } else if group.iter().all(|c| c.as_midi().is_none()) {
                self.start_audio_merge(track, &group)?;
            }
        }
        if !midi_commands.is_empty() {
            self.batch("Merge Clips", midi_commands)?;
            self.selection.select_clips(&merged, SelectMode::Replace);
        }
        Ok(())
    }

    /// The commands that put `group` (MIDI clips of `track`, by start) into
    /// one clip, and its id.
    fn merge_midi(&mut self, track: TrackId, group: &[Clip]) -> (Vec<Command>, ClipId) {
        let p = &self.project;
        let start = group[0].start;
        let end = group
            .iter()
            .map(|c| c.end(&p.timeline, p.sample_rate))
            .max()
            .unwrap_or(start);
        let mut out = MidiClip {
            length: end - start,
            ..MidiClip::default()
        };
        let mut lanes: HashMap<(faderframe_project::MidiController, u8), usize> = HashMap::new();
        for c in group {
            let Some(m) = c.as_midi() else { continue };
            let shift = c.start - start;
            for n in m.notes.iter().filter(|n| n.start < m.length) {
                let id: NoteId = self.project.ids.allocate();
                let mut moved = *n;
                moved.id = id;
                moved.start = n.start + shift;
                // A note the clip cut off stays cut off.
                moved.length = n.length.min(m.length - n.start);
                out.notes.push(moved);
                if let Some(e) = m.expressions.iter().find(|e| e.note == n.id) {
                    let mut e = e.clone();
                    e.note = id;
                    out.expressions.push(e);
                }
            }
            for l in &m.controllers {
                let i = *lanes.entry((l.controller, l.channel)).or_insert_with(|| {
                    out.controllers
                        .push(faderframe_project::ControllerLane::new(
                            l.controller,
                            l.channel,
                        ));
                    out.controllers.len() - 1
                });
                let lane = &mut out.controllers[i];
                lane.points
                    .extend(l.points.iter().filter(|pt| pt.time < m.length).map(|pt| {
                        faderframe_project::ControllerPoint {
                            time: pt.time + shift,
                            value: pt.value,
                        }
                    }));
            }
            out.sysex
                .extend(m.sysex.iter().filter(|s| s.time < m.length).map(|s| {
                    faderframe_project::SysexEvent {
                        time: s.time + shift,
                        data: s.data.clone(),
                    }
                }));
        }
        out.notes.sort_by_key(|n| (n.start, n.key));
        for l in &mut out.controllers {
            l.normalize();
        }
        out.sysex.sort_by_key(|s| s.time);
        let id: ClipId = self.project.ids.allocate();
        let mut commands: Vec<Command> = group
            .iter()
            .map(|c| Command::RemoveClip { clip: c.id })
            .collect();
        commands.push(Command::AddClip {
            clip: Box::new(Clip {
                id,
                track,
                name: group[0].name.clone(),
                color: group[0].color,
                start,
                muted: false,
                content: ClipContent::Midi(out),
            }),
        });
        (commands, id)
    }

    /// Render `group` (audio clips of `track`) into one file, in a worker.
    fn start_audio_merge(&mut self, track: TrackId, group: &[Clip]) -> Result<()> {
        let whole = self.absolute_copy();
        let layout = self
            .project
            .track(track)
            .map_or(ChannelLayout::Stereo, |t| t.layout);
        let mono = layout == ChannelLayout::Mono;
        // A project of its own: the clips on a track without devices.
        let rate = self.project.sample_rate;
        let mut p = Project::new("Merge Clips", rate);
        p.timeline = whole.timeline.clone();
        p.ids = whole.ids.clone();
        let mut t = Track::new(track, TrackKind::Audio, "Merge", TrackColor::palette(0))
            .with_layout(if mono {
                ChannelLayout::Mono
            } else {
                ChannelLayout::Stereo
            });
        for c in group {
            let Some(clip) = whole.clip(c.id) else {
                continue;
            };
            for s in clip.content.sources() {
                if let Some(src) = whole.sources.get(&s) {
                    p.sources.insert(s, src.clone());
                }
            }
            t.clips.push(c.id);
            p.clips.insert(
                c.id,
                Clip {
                    track,
                    ..clip.clone()
                },
            );
        }
        p.tracks.insert(0, t);
        let (copy, start, end) = render::track_render_project(&p, track)
            .ok_or_else(|| SessionError::Other("nothing to merge".into()))?;
        std::fs::create_dir_all(&self.media_dir)
            .map_err(|e| SessionError::Other(format!("{}: {e}", self.media_dir.display())))?;
        let name = group[0].name.clone();
        let path = crate::media::unique_path(&self.media_dir, &format!("{name} Merged"));
        let settings = RenderSettings {
            range: RenderRange::Span { start, end },
            channels: if mono {
                RenderChannels::First
            } else {
                RenderChannels::Stereo
            },
            sample_rate: rate,
            tail_seconds: 0.0,
            normalize_db: None,
            format: WavFormat::Float32,
            dither: faderframe_audio_files::Dither::Off,
            report: false,
            ..RenderSettings::defaults_for(&copy, path.clone())
        };
        let job = render::start(copy, settings).map_err(|e| SessionError::Other(e.to_string()))?;
        self.merges.push(MergeJob {
            track,
            clips: group.iter().map(|c| c.id).collect(),
            start,
            name,
            job,
            path,
        });
        Ok(())
    }

    /// Audio merges under way.
    pub fn merging(&self) -> bool {
        !self.merges.is_empty()
    }

    /// Place finished audio merges (the session's tick).
    pub(crate) fn poll_merges(&mut self) {
        let mut i = 0;
        while i < self.merges.len() {
            if !self.merges[i].job.is_finished() {
                i += 1;
                continue;
            }
            let m = self.merges.remove(i);
            let result = m
                .job
                .join()
                .map_err(|e| SessionError::Other(e.to_string()))
                .and_then(|_| self.place_merge(&m.clips, m.track, m.start, &m.name, &m.path));
            if let Err(e) = result {
                let _ = std::fs::remove_file(&m.path);
                self.notify(NoticeLevel::Error, format!("merge clips: {e}"));
            }
            self.revision += 1;
        }
    }

    /// Wait for the audio merges (tests).
    pub fn wait_for_merges(&mut self) {
        while self.merging() {
            std::thread::sleep(std::time::Duration::from_millis(5));
            self.poll_merges();
        }
    }

    fn place_merge(
        &mut self,
        clips: &[ClipId],
        track: TrackId,
        start: MusicalTime,
        name: &str,
        path: &std::path::Path,
    ) -> Result<()> {
        // Clips deleted meanwhile: their merge is no longer wanted.
        if clips.iter().any(|c| self.project.clip(*c).is_none()) {
            let _ = std::fs::remove_file(path);
            return Ok(());
        }
        let wav = faderframe_audio_files::wavstream::WavFile::open(path)
            .map_err(|e| SessionError::Other(format!("{}: {e}", path.display())))?;
        let (frames, channels, rate) = (wav.frames() as i64, wav.channels(), wav.sample_rate());
        let source = AudioSource {
            id: self.project.ids.allocate(),
            name: path
                .file_stem()
                .map_or_else(|| name.to_string(), |s| s.to_string_lossy().to_string()),
            spec: SourceSpec::File {
                path: path.to_path_buf(),
                channels: channels as u16,
                frames,
                sample_rate: rate,
            },
        };
        let id: ClipId = self.project.ids.allocate();
        let mut commands = vec![Command::AddSource {
            source: Box::new(source.clone()),
        }];
        commands.extend(clips.iter().map(|c| Command::RemoveClip { clip: *c }));
        commands.push(Command::AddClip {
            clip: Box::new(Clip {
                id,
                track,
                name: name.to_string(),
                color: None,
                start,
                muted: false,
                content: ClipContent::Audio(AudioClip {
                    source: source.id,
                    source_offset: 0,
                    length: frames.max(1),
                    gain_db: 0.0,
                    fades: ClipFades::default(),
                    stretch: StretchSettings::Off,
                    reversed: false,
                    warp: None,
                    pitch: None,
                    effects: None,
                    spectral: None,
                }),
            }),
        });
        self.batch("Merge Clips", commands)?;
        self.selection.select_clips(&[id], SelectMode::Replace);
        Ok(())
    }
}
