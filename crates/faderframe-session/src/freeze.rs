//! Freezing tracks (play rendered audio instead of clips, instrument and
//! inserts, whose plugins are unloaded) and bouncing them to a new audio
//! track. Both render the track after its inserts, before its fader, on a
//! worker thread; the session finishes the job in its tick.

use crate::render::{self, RenderChannels, RenderJob, RenderRange, RenderSettings};
use crate::{NoticeLevel, Result, Session, SessionError};
use faderframe_audio_files::WavFormat;
use faderframe_core::{ChannelLayout, ClipId, TrackId};
use faderframe_project::{
    AudioClip, AudioSource, Clip, ClipContent, ClipFades, Command, Freeze, SourceSpec,
    StretchSettings, Track, TrackColor, TrackKind,
};
use faderframe_timeline::MusicalTime;
use std::path::PathBuf;

/// Seconds rendered after the last clip (reverb and delay tails).
const TAIL_SECONDS: f32 = 2.0;

pub(crate) struct PendingBounce {
    job: RenderJob,
    track: TrackId,
    freeze: bool,
    start: MusicalTime,
    path: PathBuf,
}

impl Session {
    /// Is the track frozen?
    pub fn is_frozen(&self, track: TrackId) -> bool {
        self.project
            .track(track)
            .is_some_and(|t| t.freeze.is_some())
    }

    /// Track renders still running.
    pub fn bouncing(&self) -> Vec<TrackId> {
        self.bounces.iter().map(|b| b.track).collect()
    }

    /// Render `track` in the background, then freeze it or put the audio on
    /// a new track.
    pub(crate) fn start_bounce(&mut self, track: TrackId, freeze: bool) -> Result<()> {
        if self.bounces.iter().any(|b| b.track == track) {
            return Ok(());
        }
        let t = self
            .project
            .track(track)
            .ok_or_else(|| SessionError::Other(format!("no track {track}")))?;
        if !matches!(t.kind, TrackKind::Audio | TrackKind::Instrument) {
            return Err(SessionError::Other(format!(
                "'{}' has no clips to render",
                t.name
            )));
        }
        if freeze && !self.output_tracks_of(track).is_empty() {
            return Err(SessionError::Other(format!(
                "'{}' feeds output tracks from its plugin: freezing it would silence them",
                t.name
            )));
        }
        let name = t.name.clone();
        // What the chain makes: a mono track widened by a device (the
        // Guitar Station) freezes and bounces stereo.
        let mono = t.chain_layout() == ChannelLayout::Mono;
        let project = self.absolute_copy();
        let (copy, start, end) = render::track_render_project(&project, track)
            .ok_or_else(|| SessionError::Other(format!("'{name}' has no clips to render")))?;
        std::fs::create_dir_all(&self.media_dir)
            .map_err(|e| SessionError::Other(format!("{}: {e}", self.media_dir.display())))?;
        let stem = format!("{name} {}", if freeze { "Freeze" } else { "Bounce" });
        let path = crate::media::unique_path(&self.media_dir, &stem);
        let settings = RenderSettings {
            range: RenderRange::Span { start, end },
            channels: if mono {
                RenderChannels::First
            } else {
                RenderChannels::Stereo
            },
            sample_rate: self.project.sample_rate,
            tail_seconds: TAIL_SECONDS,
            normalize_db: None,
            format: WavFormat::Float32,
            dither: faderframe_audio_files::Dither::Off,
            report: false,
            // A freeze plays its file with the latency it was made with
            // (exactly as the track did); a bounce starts on time.
            keep_latency: freeze,
            ..RenderSettings::defaults_for(&copy, path.clone())
        };
        let job = render::start(copy, settings).map_err(|e| SessionError::Other(e.to_string()))?;
        self.bounces.push(PendingBounce {
            job,
            track,
            freeze,
            start,
            path,
        });
        self.notify(
            NoticeLevel::Info,
            format!("{} '{name}'…", if freeze { "freezing" } else { "bouncing" }),
        );
        self.revision += 1;
        Ok(())
    }

    /// A copy of the project to render: the plugins' current settings
    /// captured, every media path absolute.
    pub(crate) fn absolute_copy(&mut self) -> faderframe_project::Project {
        self.capture_plugin_states();
        let mut project = self.project.clone();
        for s in project.sources.values_mut() {
            if let SourceSpec::File { path, .. } = &mut s.spec
                && path.is_relative()
                && let Some(dir) = self.project_dir()
            {
                *path = dir.join(&*path);
            }
        }
        project
    }

    /// Unfreeze: the clips, instrument and inserts play again.
    pub(crate) fn unfreeze(&mut self, track: TrackId) -> Result<()> {
        if !self.is_frozen(track) {
            return Ok(());
        }
        self.edit(Command::SetTrackFreeze {
            track,
            freeze: None,
        })
    }

    /// Finish renders that are done (from the tick).
    pub(crate) fn poll_bounces(&mut self) {
        let mut i = 0;
        while i < self.bounces.len() {
            if !self.bounces[i].job.is_finished() {
                i += 1;
                continue;
            }
            let b = self.bounces.remove(i);
            let latency = b
                .job
                .progress
                .latency
                .load(std::sync::atomic::Ordering::Relaxed);
            let result = b.job.join();
            if let Err(e) = result
                .map_err(|e| SessionError::Other(e.to_string()))
                .and_then(|_| self.finish_bounce(b.track, b.freeze, b.start, b.path, latency))
            {
                self.notify(NoticeLevel::Error, format!("bounce: {e}"));
            }
            self.revision += 1;
        }
    }

    fn finish_bounce(
        &mut self,
        track: TrackId,
        freeze: bool,
        start: MusicalTime,
        path: PathBuf,
        latency: u32,
    ) -> Result<()> {
        let wav = faderframe_audio_files::wavstream::WavFile::open(&path)
            .map_err(|e| SessionError::Other(format!("{}: {e}", path.display())))?;
        let (frames, channels, rate) = (wav.frames() as i64, wav.channels(), wav.sample_rate());
        let Some(t) = self.project.track(track).cloned() else {
            return Ok(()); // deleted meanwhile
        };
        let p = &mut self.project;
        let source = AudioSource {
            id: p.ids.allocate(),
            name: path
                .file_stem()
                .map_or_else(|| t.name.clone(), |s| s.to_string_lossy().to_string()),
            spec: SourceSpec::File {
                path: path.clone(),
                channels: channels as u16,
                frames,
                sample_rate: rate,
            },
        };
        let source_id = source.id;
        let mut cmds = vec![Command::AddSource {
            source: Box::new(source),
        }];
        if freeze {
            cmds.push(Command::SetTrackFreeze {
                track,
                freeze: Some(Freeze {
                    source: source_id,
                    start,
                    length: frames,
                    latency,
                }),
            });
            self.batch("Freeze Track", cmds)?;
            self.notify(NoticeLevel::Info, format!("froze '{}'", t.name));
        } else {
            let id: TrackId = p.ids.allocate();
            let clip: ClipId = p.ids.allocate();
            let index = p.track_index(track).map_or(p.tracks.len(), |i| i + 1);
            let mut new = Track::new(
                id,
                TrackKind::Audio,
                format!("{} Bounce", t.name),
                TrackColor::palette(p.tracks.len()),
            )
            .with_layout(t.chain_layout());
            new.output = t.output;
            new.volume_db = t.volume_db;
            new.pan = t.pan;
            cmds.push(Command::AddTrack {
                track: Box::new(new),
                index,
            });
            cmds.push(Command::AddClip {
                clip: Box::new(Clip {
                    id: clip,
                    track: id,
                    name: format!("{} Bounce", t.name),
                    color: None,
                    start,
                    muted: false,
                    content: ClipContent::Audio(AudioClip {
                        source: source_id,
                        source_offset: 0,
                        length: frames,
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
            cmds.push(Command::SetTrackMute { track, on: true });
            self.batch("Bounce Track", cmds)?;
            self.notify(
                NoticeLevel::Info,
                format!(
                    "bounced '{}' to a new track (the original is muted)",
                    t.name
                ),
            );
        }
        Ok(())
    }

    /// The frozen track an edit would change (edits of frozen tracks are
    /// refused until they are unfrozen).
    pub(crate) fn frozen_target(&self, cmd: &Command) -> Option<String> {
        let track_of_clip = |c: &ClipId| self.project.clip(*c).map(|c| c.track);
        let frozen = |t: Option<TrackId>| {
            t.and_then(|t| self.project.track(t))
                .filter(|t| t.freeze.is_some())
                .map(|t| t.name.clone())
        };
        match cmd {
            Command::Batch { commands, .. } => commands.iter().find_map(|c| self.frozen_target(c)),
            Command::AddClip { clip } => frozen(Some(clip.track)),
            Command::MoveClip { clip, track, .. } => {
                frozen(track_of_clip(clip)).or_else(|| frozen(Some(*track)))
            }
            Command::RemoveClip { clip }
            | Command::SetClipContent { clip, .. }
            | Command::SetClipMuted { clip, .. }
            | Command::SplitClip { clip, .. }
            | Command::AddNote { clip, .. }
            | Command::RemoveNote { clip, .. }
            | Command::UpdateNote { clip, .. } => frozen(track_of_clip(clip)),
            Command::InsertPlugin { track, .. }
            | Command::RemovePlugin { track, .. }
            | Command::SetPreamp { track, .. }
            | Command::SetInstrument { track, .. }
            | Command::SetPluginState { track, .. } => frozen(Some(*track)),
            _ => None,
        }
    }
}
