//! Redrawing audio with the Pencil at sample level (click repair). Never
//! destructive: the source is copied with the drawn samples into a new
//! file in the project's media folder — on a thread, the stroke shown
//! meanwhile ([`Session::pending_redraws`]) — and only the edited clip
//! switches to it (one undo step brings the original back).

use crate::{NoticeLevel, Result, Session, SessionError};
use faderframe_audio_files::WavFormat;
use faderframe_audio_files::wavstream::{WavFile, WavWriter};
use faderframe_core::{AudioSourceId, ClipId};
use faderframe_engine::Source;
use faderframe_project::{AudioSource, ClipContent, Command, SourceSpec};
use std::path::{Path, PathBuf};
use std::sync::Arc;

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
    /// (one channel, or all with `None`) — in a copy the clip then uses,
    /// written in the background (strokes queue: each copies the source
    /// the clip has when it starts, so they add up).
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
            .ok_or_else(|| SessionError::Other(format!("no clip {clip}")))?;
        if !matches!(c.content, ClipContent::Audio(_)) {
            return Err(SessionError::Other(
                "only audio clips can be redrawn".into(),
            ));
        }
        if samples.is_empty() {
            return Ok(());
        }
        self.redraw.queue.push_back(Stroke {
            clip,
            channel,
            start,
            samples: samples.to_vec(),
        });
        self.start_redraw();
        Ok(())
    }

    /// Strokes drawn on `clip` and not in its audio yet: (channel, first
    /// source frame, values) — views keep showing them.
    pub fn pending_redraws(&self, clip: ClipId) -> Vec<(Option<usize>, i64, &[f32])> {
        self.redraw
            .running
            .iter()
            .map(|r| &r.stroke)
            .chain(&self.redraw.queue)
            .filter(|s| s.clip == clip)
            .map(|s| (s.channel, s.start, s.samples.as_slice()))
            .collect()
    }

    /// Whether redrawn audio is still being written.
    pub fn redrawing(&self) -> bool {
        self.redraw.running.is_some() || !self.redraw.queue.is_empty()
    }

    /// Wait for redrawn audio to be in the clips (tests).
    pub fn wait_for_redraws(&mut self) {
        let t = std::time::Instant::now();
        while self.redrawing() && t.elapsed() < std::time::Duration::from_secs(60) {
            self.poll_redraws();
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    /// Start the next stroke's copy (when none is being written).
    fn start_redraw(&mut self) {
        while self.redraw.running.is_none() {
            let Some(stroke) = self.redraw.queue.pop_front() else {
                return;
            };
            match self.redraw_job(stroke) {
                Ok(r) => self.redraw.running = Some(r),
                Err(e) => self.notify(NoticeLevel::Error, format!("Redraw: {e}")),
            }
        }
    }

    /// The copies a stroke needs (the clip's source, and the unedited one
    /// under spectral edits), on a thread.
    fn redraw_job(&mut self, stroke: Stroke) -> Result<Running> {
        let c = self
            .project
            .clip(stroke.clip)
            .ok_or_else(|| SessionError::Other("the clip is gone".into()))?;
        let ClipContent::Audio(audio) = &c.content else {
            return Err(SessionError::Other("not an audio clip".into()));
        };
        let mut from = vec![audio.source];
        if let Some(sp) = &audio.spectral {
            from.push(sp.original);
        }
        std::fs::create_dir_all(&self.media_dir)
            .map_err(|e| SessionError::Other(format!("{}: {e}", self.media_dir.display())))?;
        let mut copies = Vec::new();
        for source in &from {
            let src = self
                .sources
                .get(source)
                .ok_or_else(|| SessionError::Other("the clip's audio is not loaded".into()))?;
            let (input, channels, frames, rate) = match src {
                Source::Memory(d) => (
                    Input::Memory(Arc::clone(d)),
                    d.num_channels(),
                    d.frames() as i64,
                    self.engine.sample_rate(),
                ),
                Source::Stream(s) => (
                    Input::File(s.path().to_path_buf()),
                    s.channels(),
                    s.frames() as i64,
                    s.sample_rate(),
                ),
            };
            let name = self
                .project
                .sources
                .get(source)
                .map_or_else(|| "Audio".to_string(), |s| s.name.clone());
            let stem = format!("{} Redraw", name.trim_end_matches(" Redraw"));
            // Claimed now: the next stroke's copy gets another name.
            let path = crate::media::unique_path(&self.media_dir, &stem);
            std::fs::File::create(&path)
                .map_err(|e| SessionError::Other(format!("{}: {e}", path.display())))?;
            copies.push(Copy {
                input,
                path,
                stem,
                channels,
                frames,
                rate,
            });
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let job: Vec<_> = copies
            .iter()
            .map(|c| {
                (
                    c.input.clone(),
                    c.path.clone(),
                    c.channels,
                    c.frames,
                    c.rate,
                )
            })
            .collect();
        let (channel, start, samples) = (stroke.channel, stroke.start, stroke.samples.clone());
        std::thread::Builder::new()
            .name("faderframe-redraw".into())
            .spawn(move || {
                let r = job
                    .iter()
                    .try_for_each(|(input, path, channels, frames, rate)| {
                        write_copy(
                            input, path, *channels, *frames, *rate, channel, start, &samples,
                        )
                    });
                let _ = tx.send(r);
            })
            .map_err(|e| SessionError::Other(format!("cannot start the redraw: {e}")))?;
        Ok(Running {
            stroke,
            from,
            copies,
            done: rx,
        })
    }

    /// Put a finished copy into its clip (one undo step), then go on.
    pub(crate) fn poll_redraws(&mut self) {
        let Some(r) = &self.redraw.running else {
            return;
        };
        let result = match r.done.try_recv() {
            Ok(result) => result,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Err("the redraw stopped".to_string())
            }
        };
        let Some(r) = self.redraw.running.take() else {
            return;
        };
        if let Err(e) = result
            .map_err(SessionError::Other)
            .and_then(|()| self.apply_redraw(&r))
        {
            for c in &r.copies {
                let _ = std::fs::remove_file(&c.path);
            }
            self.notify(NoticeLevel::Error, format!("Redraw: {e}"));
        }
        self.revision += 1;
        self.start_redraw();
    }

    fn apply_redraw(&mut self, r: &Running) -> Result<()> {
        let c = self
            .project
            .clip(r.stroke.clip)
            .cloned()
            .ok_or_else(|| SessionError::Other("the clip is gone".into()))?;
        let ClipContent::Audio(audio) = &c.content else {
            return Err(SessionError::Other("the clip changed".into()));
        };
        // Edited meanwhile (undone, replaced): the drawing is dropped.
        if audio.source != r.from[0]
            || audio.spectral.as_ref().map(|sp| sp.original) != r.from.get(1).copied()
        {
            return Err(SessionError::Other(
                "the clip changed while it was redrawn".into(),
            ));
        }
        let mut content = audio.clone();
        let mut commands = Vec::new();
        for (k, copy) in r.copies.iter().enumerate() {
            let id: AudioSourceId = self.project.ids.allocate();
            commands.push(Command::AddSource {
                source: Box::new(AudioSource {
                    id,
                    name: copy.stem.clone(),
                    spec: SourceSpec::File {
                        path: copy.path.clone(),
                        channels: copy.channels as u16,
                        frames: copy.frames,
                        sample_rate: copy.rate,
                    },
                }),
            });
            // Spectral edits render from the unedited source: it is
            // redrawn too, so the drawing stays under them.
            match (k, content.spectral.as_mut()) {
                (0, _) => content.source = id,
                (_, Some(sp)) => sp.original = id,
                _ => {}
            }
        }
        commands.push(Command::SetClipContent {
            clip: c.id,
            start: c.start,
            content: Box::new(ClipContent::Audio(content)),
        });
        self.edit(Command::Batch {
            label: "Redraw Waveform".into(),
            commands,
        })?;
        self.notify(
            NoticeLevel::Info,
            format!("redrew {} samples of '{}'", r.stroke.samples.len(), c.name),
        );
        Ok(())
    }
}

/// Strokes waiting and the copy being written.
#[derive(Default)]
pub(crate) struct RedrawState {
    queue: std::collections::VecDeque<Stroke>,
    running: Option<Running>,
}

struct Stroke {
    clip: ClipId,
    channel: Option<usize>,
    start: i64,
    samples: Vec<f32>,
}

struct Running {
    stroke: Stroke,
    /// The sources copied (the clip's, then the unedited one).
    from: Vec<AudioSourceId>,
    copies: Vec<Copy>,
    done: std::sync::mpsc::Receiver<std::result::Result<(), String>>,
}

struct Copy {
    input: Input,
    path: PathBuf,
    stem: String,
    channels: usize,
    frames: i64,
    rate: u32,
}

/// Where a copy reads (its own handle on a file: never the engine's
/// stream).
#[derive(Clone)]
enum Input {
    Memory(Arc<faderframe_audio_files::AudioData>),
    File(PathBuf),
}

impl Input {
    /// Frames from `from` into `out` (all channels; zero past the end).
    fn read(
        &self,
        file: &mut Option<WavFile>,
        from: u64,
        out: &mut [Vec<f32>],
    ) -> std::result::Result<(), String> {
        match self {
            Input::Memory(d) => {
                for (c, dst) in out.iter_mut().enumerate() {
                    let ch = d.channel(c);
                    for (i, v) in dst.iter_mut().enumerate() {
                        *v = ch.get(from as usize + i).copied().unwrap_or(0.0);
                    }
                }
            }
            Input::File(path) => {
                if file.is_none() {
                    *file =
                        Some(WavFile::open(path).map_err(|e| format!("{}: {e}", path.display()))?);
                }
                if let Some(f) = file {
                    let mut refs: Vec<&mut [f32]> = out.iter_mut().map(|c| &mut c[..]).collect();
                    f.read(from, &mut refs, &mut Vec::new())
                        .map_err(|e| format!("{}: {e}", path.display()))?;
                }
            }
        }
        Ok(())
    }
}

/// Copy `input` into `path` with `samples` drawn in from frame `start`
/// (one channel, or all).
#[allow(clippy::too_many_arguments)]
fn write_copy(
    input: &Input,
    path: &Path,
    channels: usize,
    frames: i64,
    rate: u32,
    channel: Option<usize>,
    start: i64,
    samples: &[f32],
) -> std::result::Result<(), String> {
    let io = |e: std::io::Error| format!("{}: {e}", path.display());
    let mut w =
        WavWriter::create(path, channels as u16, rate, WavFormat::Float32, false).map_err(io)?;
    let end = start + samples.len() as i64;
    let mut chunk = vec![vec![0.0f32; CHUNK]; channels];
    let mut file = None;
    let mut at = 0i64;
    while at < frames {
        let n = CHUNK.min((frames - at) as usize);
        for ch in chunk.iter_mut() {
            ch.resize(n, 0.0);
        }
        input.read(&mut file, at as u64, &mut chunk)?;
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
    w.finish().map(drop).map_err(io)
}
