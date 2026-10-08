//! Redrawing audio with the Pencil at sample level (click repair). Never
//! destructive: the source is copied with the drawn samples into a new
//! file in the project's media folder, and only the edited clip switches
//! to it (one undo step brings the original back).

use crate::{NoticeLevel, Result, Session, SessionError};
use faderframe_audio_files::WavFormat;
use faderframe_audio_files::wavstream::{WavFile, WavWriter};
use faderframe_core::{AudioSourceId, ClipId};
use faderframe_engine::Source;
use faderframe_project::{AudioSource, ClipContent, Command, SourceSpec};

/// Frames copied per step.
const CHUNK: usize = 1 << 16;

impl Session {
    /// The source's own frames `start..start + count` of every channel,
    /// zero outside the source (`None`: not loaded).
    pub fn source_frames(
        &self,
        source: AudioSourceId,
        start: i64,
        count: usize,
    ) -> Option<Vec<Vec<f32>>> {
        let mut out = vec![vec![0.0f32; count]; self.sources.get(&source)?.channels()];
        self.read_source(source, start, &mut out).ok()?;
        Some(out)
    }

    fn read_source(&self, source: AudioSourceId, start: i64, out: &mut [Vec<f32>]) -> Result<()> {
        let src = self
            .sources
            .get(&source)
            .ok_or_else(|| SessionError::Other(format!("source {source} is not loaded")))?;
        let count = out.first().map_or(0, Vec::len);
        // Frames before the source start stay zero.
        let skip = (-start).clamp(0, count as i64) as usize;
        let from = start.max(0);
        match src {
            Source::Memory(d) => {
                for (c, dst) in out.iter_mut().enumerate() {
                    let ch = d.channel(c);
                    for (i, v) in dst.iter_mut().enumerate().skip(skip) {
                        let f = from as usize + (i - skip);
                        *v = ch.get(f).copied().unwrap_or(0.0);
                    }
                }
            }
            Source::Stream(s) => {
                let f = WavFile::open(s.path())
                    .map_err(|e| SessionError::Other(format!("{}: {e}", s.path().display())))?;
                let mut refs: Vec<&mut [f32]> = out.iter_mut().map(|c| &mut c[skip..]).collect();
                f.read(from as u64, &mut refs, &mut Vec::new())
                    .map_err(|e| SessionError::Other(format!("{}: {e}", s.path().display())))?;
            }
        }
        Ok(())
    }

    /// Replace `samples` of the clip's source from source frame `start`
    /// (one channel, or all with `None`) — in a copy the clip then uses.
    pub(crate) fn redraw_audio(
        &mut self,
        clip: ClipId,
        channel: Option<usize>,
        start: i64,
        samples: &[f32],
    ) -> Result<()> {
        let c = self
            .project
            .clip(clip)
            .cloned()
            .ok_or_else(|| SessionError::Other(format!("no clip {clip}")))?;
        let ClipContent::Audio(audio) = &c.content else {
            return Err(SessionError::Other(
                "only audio clips can be redrawn".into(),
            ));
        };
        if samples.is_empty() {
            return Ok(());
        }
        let (id, source) = self.redrawn_copy(audio.source, channel, start, samples)?;
        let mut content = audio.clone();
        content.source = id;
        let mut commands = vec![Command::AddSource {
            source: Box::new(source),
        }];
        // Spectral edits render from the unedited source: it is redrawn
        // too, so the drawing stays under them.
        if let Some(sp) = content.spectral.as_mut() {
            let (id, source) = self.redrawn_copy(sp.original, channel, start, samples)?;
            sp.original = id;
            commands.push(Command::AddSource {
                source: Box::new(source),
            });
        }
        commands.push(Command::SetClipContent {
            clip,
            start: c.start,
            content: Box::new(ClipContent::Audio(content)),
        });
        self.edit(Command::Batch {
            label: "Redraw Waveform".into(),
            commands,
        })?;
        self.notify(
            NoticeLevel::Info,
            format!("redrew {} samples of '{}'", samples.len(), c.name),
        );
        Ok(())
    }

    /// A copy of `source` with `samples` drawn in from frame `start` (one
    /// channel, or all), as a new source (not yet in the project).
    fn redrawn_copy(
        &mut self,
        source: AudioSourceId,
        channel: Option<usize>,
        start: i64,
        samples: &[f32],
    ) -> Result<(AudioSourceId, AudioSource)> {
        let src = self
            .sources
            .get(&source)
            .ok_or_else(|| SessionError::Other("the clip's audio is not loaded".into()))?;
        let (channels, frames, rate) = match src {
            Source::Memory(d) => (
                d.num_channels(),
                d.frames() as i64,
                self.engine.sample_rate(),
            ),
            Source::Stream(s) => (s.channels(), s.frames() as i64, s.sample_rate()),
        };
        let name = self
            .project
            .sources
            .get(&source)
            .map_or_else(|| "Audio".to_string(), |s| s.name.clone());
        std::fs::create_dir_all(&self.media_dir)
            .map_err(|e| SessionError::Other(format!("{}: {e}", self.media_dir.display())))?;
        let stem = format!("{} Redraw", name.trim_end_matches(" Redraw"));
        let path = crate::media::unique_path(&self.media_dir, &stem);
        let io = |e: std::io::Error| SessionError::Other(format!("{}: {e}", path.display()));
        let mut w = WavWriter::create(&path, channels as u16, rate, WavFormat::Float32, false)
            .map_err(io)?;
        let end = start + samples.len() as i64;
        let mut chunk = vec![vec![0.0f32; CHUNK]; channels];
        let mut at = 0i64;
        while at < frames {
            let n = CHUNK.min((frames - at) as usize);
            for ch in chunk.iter_mut() {
                ch.resize(n, 0.0);
            }
            self.read_source(source, at, &mut chunk)?;
            // The drawn samples.
            let (a, b) = (start.max(at), end.min(at + n as i64));
            for f in a..b {
                let v = samples[(f - start) as usize].clamp(-1.0, 1.0);
                for (c, ch) in chunk.iter_mut().enumerate() {
                    if channel.is_none_or(|k| k == c) {
                        ch[(f - at) as usize] = v;
                    }
                }
            }
            let refs: Vec<&[f32]> = chunk.iter().map(|c| &c[..n]).collect();
            w.write_planar(&refs, n).map_err(io)?;
            at += n as i64;
        }
        w.finish().map_err(io)?;
        let id: AudioSourceId = self.project.ids.allocate();
        Ok((
            id,
            AudioSource {
                id,
                name: stem,
                spec: SourceSpec::File {
                    path: path.clone(),
                    channels: channels as u16,
                    frames,
                    sample_rate: rate,
                },
            },
        ))
    }
}
