//! Speech and lyrics: a clip's words transcribed into the project's
//! lyrics (see [`faderframe_project::lyrics`]).
//!
//! The model ([`faderframe_speech`], OpenAI's Whisper) is not shipped: its
//! checkpoint (`whisper-base`, 290 MB, Apache-2.0) is downloaded once, on
//! request, into the data folder's `models` (by the system's `curl`, file
//! by file through `.part` files). Transcribing runs in a thread on the
//! clip's part of its source at 16 kHz; its lines replace those starting
//! inside the clip, in one undo step.

use crate::pitch::mono_of;
use crate::{NoticeLevel, Result, Session, SessionError};
use faderframe_core::ClipId;
use faderframe_project::Command;
use faderframe_project::lyrics::{self, LyricLine};
use faderframe_speech::{Options, Segment, Whisper, files};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread::JoinHandle;

/// The checkpoint used.
pub const CHECKPOINT: &str = "whisper-base";
/// Its size, for asking.
pub const CHECKPOINT_MB: u32 = 290;

type Download = JoinHandle<std::result::Result<(), String>>;
/// A transcription running: its clip, the segments it will hear, how far
/// it is (per mille).
type Job = (
    ClipId,
    JoinHandle<std::result::Result<Vec<Segment>, String>>,
    Arc<AtomicU32>,
);

#[derive(Default)]
pub(crate) struct SpeechState {
    download: Option<(Download, Arc<AtomicU32>)>,
    jobs: Vec<Job>,
}

/// Where the checkpoint lives.
pub fn model_dir() -> PathBuf {
    crate::media::data_dir().join("models").join(CHECKPOINT)
}

/// Is the checkpoint on this computer?
pub fn model_ready() -> bool {
    let dir = model_dir();
    files::FILES.iter().all(|f| dir.join(f).is_file())
}

fn download(progress: &AtomicU32) -> std::result::Result<(), String> {
    let dir = model_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for (i, f) in files::FILES.iter().enumerate() {
        let target = dir.join(f);
        if target.is_file() {
            progress.store(i as u32 + 1, Ordering::Relaxed);
            continue;
        }
        let part = dir.join(format!("{f}.part"));
        let status = std::process::Command::new("curl")
            .args([
                "--location",
                "--fail",
                "--silent",
                "--show-error",
                "--retry",
                "3",
                "--output",
            ])
            .arg(&part)
            .arg(files::url(CHECKPOINT, f))
            .status()
            .map_err(|e| {
                format!(
                    "curl: {e} (download {} by hand into {})",
                    files::url(CHECKPOINT, f),
                    dir.display()
                )
            })?;
        if !status.success() {
            let _ = std::fs::remove_file(&part);
            return Err(format!("could not download {f} ({status})"));
        }
        std::fs::rename(&part, &target).map_err(|e| format!("{}: {e}", target.display()))?;
        progress.store(i as u32 + 1, Ordering::Relaxed);
    }
    Ok(())
}

impl Session {
    /// Download the speech model (once; a notice says when it is there).
    pub fn download_speech_model(&mut self) -> Result<()> {
        if model_ready() {
            self.notify(NoticeLevel::Info, "The speech model is here already");
            return Ok(());
        }
        if self.speech.download.is_some() {
            return Ok(());
        }
        let progress = Arc::new(AtomicU32::new(0));
        let p = Arc::clone(&progress);
        let job = std::thread::Builder::new()
            .name("faderframe-model-download".into())
            .spawn(move || download(&p))
            .map_err(|e| SessionError::Other(e.to_string()))?;
        self.speech.download = Some((job, progress));
        self.notify(
            NoticeLevel::Info,
            format!("Downloading the speech model ({CHECKPOINT}, {CHECKPOINT_MB} MB)…"),
        );
        self.revision += 1;
        Ok(())
    }

    /// The download's progress (files done of all), while it runs.
    pub fn speech_download(&self) -> Option<(u32, u32)> {
        self.speech
            .download
            .as_ref()
            .map(|(_, p)| (p.load(Ordering::Relaxed), files::FILES.len() as u32))
    }

    /// Transcribe a clip's words into the lyrics.
    pub fn transcribe(&mut self, clip: ClipId) -> Result<()> {
        if !model_ready() {
            return Err(SessionError::Other(format!(
                "the speech model is not on this computer yet: Audio → Download Speech Model ({CHECKPOINT_MB} MB)"
            )));
        }
        let c = self
            .project
            .clip(clip)
            .cloned()
            .ok_or_else(|| SessionError::Other(format!("no clip {clip}")))?;
        let a = c
            .as_audio()
            .ok_or_else(|| SessionError::Other("only audio clips have words to hear".into()))?;
        let source = self
            .sources
            .get(&a.source)
            .cloned()
            .ok_or_else(|| SessionError::Other("the clip's audio is missing".into()))?;
        let (from, span) = (a.source_offset, a.source_span());
        let project_rate = self.project.sample_rate as f64;
        let progress = Arc::new(AtomicU32::new(0));
        let p = Arc::clone(&progress);
        let job = std::thread::Builder::new()
            .name("faderframe-transcribe".into())
            .spawn(move || {
                let (mono, rate) = mono_of(&source).ok_or("the clip's audio could not be read")?;
                let k = rate / project_rate;
                let a = ((from as f64 * k) as usize).min(mono.len());
                let b = (((from + span) as f64 * k) as usize).clamp(a, mono.len());
                let audio = crate::to_midi::resample(
                    &mono[a..b],
                    rate,
                    faderframe_speech::mel::RATE as f64,
                )
                .ok_or("the clip's audio could not be resampled")?;
                let w = Whisper::load(&model_dir()).map_err(|e| e.to_string())?;
                Ok(w.transcribe(&audio, &Options::default(), |f| {
                    p.store((f * 1000.0) as u32, Ordering::Relaxed);
                }))
            })
            .map_err(|e| SessionError::Other(e.to_string()))?;
        self.speech.jobs.push((clip, job, progress));
        self.notify(
            NoticeLevel::Info,
            format!("Listening to the words in ‘{}’…", c.name),
        );
        self.revision += 1;
        Ok(())
    }

    /// Transcriptions running.
    pub fn transcribing(&self) -> bool {
        !self.speech.jobs.is_empty()
    }

    /// Finish downloads and transcriptions (from the session tick).
    pub(crate) fn poll_speech(&mut self) {
        if self
            .speech
            .download
            .as_ref()
            .is_some_and(|(j, _)| j.is_finished())
            && let Some((job, _)) = self.speech.download.take()
        {
            match job.join() {
                Ok(Ok(())) => self.notify(
                    NoticeLevel::Info,
                    "The speech model is here: Transcribe a clip from its menu",
                ),
                Ok(Err(e)) => self.notify(NoticeLevel::Error, format!("speech model: {e}")),
                Err(_) => self.notify(NoticeLevel::Error, "speech model: the download failed"),
            }
            self.revision += 1;
        }
        let mut i = 0;
        while i < self.speech.jobs.len() {
            if !self.speech.jobs[i].1.is_finished() {
                i += 1;
                continue;
            }
            let (clip, job, _) = self.speech.jobs.swap_remove(i);
            let result = match job.join() {
                Ok(Ok(segments)) => self.place_lyrics(clip, &segments),
                Ok(Err(e)) => Err(SessionError::Other(e)),
                Err(_) => Err(SessionError::Other("the transcription failed".into())),
            };
            if let Err(e) = result {
                self.notify(NoticeLevel::Error, format!("transcribe: {e}"));
            }
            self.revision += 1;
        }
    }

    /// The segments (seconds into the clip's source part) as lyric lines
    /// replacing those inside the clip.
    fn place_lyrics(&mut self, clip: ClipId, segments: &[Segment]) -> Result<()> {
        let Some(c) = self.project.clip(clip).cloned() else {
            return Ok(());
        };
        let Some(a) = c.as_audio().cloned() else {
            return Ok(());
        };
        let p = &self.project;
        let rate = p.sample_rate as f64;
        let base = p.timeline.to_samples(c.start, rate);
        let at = |s: f64| {
            let f = a.source_offset + (s * rate).round() as i64;
            let out = match &a.warp {
                Some(w) => w.output_of(a.source_offset, a.length, f),
                None => f - a.source_offset,
            };
            p.timeline.to_musical(base + out.clamp(0, a.length), rate)
        };
        let new: Vec<LyricLine> = segments
            .iter()
            .map(|s| LyricLine {
                start: at(s.start),
                end: at(s.end),
                text: s.text.clone(),
            })
            .collect();
        let n = new.len();
        let end = c.end(&p.timeline, p.sample_rate);
        let lyrics = lyrics::replace_span(&p.lyrics, c.start, end, new);
        self.batch("Transcribe", vec![Command::SetLyrics { lyrics }])?;
        self.notify(
            NoticeLevel::Info,
            if n == 0 {
                format!("No words heard in ‘{}’", c.name)
            } else {
                format!(
                    "‘{}’: {n} line{} in the Lyrics lane",
                    c.name,
                    if n == 1 { "" } else { "s" }
                )
            },
        );
        Ok(())
    }

    /// Change (or with `None` remove) a lyric line's words.
    pub fn edit_lyric(&mut self, index: usize, text: Option<String>) -> Result<()> {
        let mut lyrics = self.project.lyrics.clone();
        match text {
            Some(t) if index < lyrics.len() => lyrics[index].text = t,
            None if index < lyrics.len() => {
                lyrics.remove(index);
            }
            _ => return Ok(()),
        }
        self.edit(Command::SetLyrics { lyrics })
    }

    /// Write the lyrics as `<project>.lrc` and `.srt` next to the project
    /// (in the media folder while it is unsaved); returns the LRC's path.
    pub fn export_lyrics(&mut self) -> Result<PathBuf> {
        if self.project.lyrics.is_empty() {
            return Err(SessionError::Other("there are no lyrics to export".into()));
        }
        let dir = self
            .project_dir()
            .unwrap_or_else(|| self.media_dir().to_path_buf());
        std::fs::create_dir_all(&dir)
            .map_err(|e| SessionError::Other(format!("{}: {e}", dir.display())))?;
        let stem = self.project.name.clone();
        let lrc = dir.join(format!("{stem}.lrc"));
        let srt = dir.join(format!("{stem}.srt"));
        for (path, text) in [
            (&lrc, self.lyrics_text(false)),
            (&srt, self.lyrics_text(true)),
        ] {
            std::fs::write(path, text)
                .map_err(|e| SessionError::Other(format!("{}: {e}", path.display())))?;
        }
        self.notify(
            NoticeLevel::Info,
            format!("Lyrics written to {} and .srt", lrc.display()),
        );
        Ok(lrc)
    }

    /// The lyrics as LRC (`[mm:ss.xx]line`) or SRT text.
    pub fn lyrics_text(&self, srt: bool) -> String {
        let p = &self.project;
        let rate = p.sample_rate as f64;
        let secs = |t| p.timeline.to_samples(t, rate) as f64 / rate;
        let mut out = String::new();
        for (i, l) in p.lyrics.iter().enumerate() {
            let (a, b) = (secs(l.start), secs(l.end));
            if srt {
                let ts = |s: f64| {
                    let ms = (s * 1000.0).round() as i64;
                    format!(
                        "{:02}:{:02}:{:02},{:03}",
                        ms / 3_600_000,
                        ms / 60_000 % 60,
                        ms / 1000 % 60,
                        ms % 1000
                    )
                };
                out.push_str(&format!(
                    "{}\n{} --> {}\n{}\n\n",
                    i + 1,
                    ts(a),
                    ts(b),
                    l.text
                ));
            } else {
                let cs = (a * 100.0).round() as i64;
                out.push_str(&format!(
                    "[{:02}:{:02}.{:02}]{}\n",
                    cs / 6000,
                    cs / 100 % 60,
                    cs % 100,
                    l.text
                ));
            }
        }
        out
    }
}
