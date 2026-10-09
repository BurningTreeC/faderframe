//! Polyphonic pitch editing in the session (the Pitch editor's
//! Polyphonic option, on by default).
//!
//! Detection: the clip's audio (its original source: a render is never
//! analysed again) through the transcription model (`to_midi::harmony`:
//! every note, chords too) and each note followed to its pitch frame by
//! frame (`faderframe_polypitch::pitch_tracks`) — a [`PitchEdit`] whose
//! notes may overlap, marked [`Polyphonic`], in one "Detect Pitch" step.
//!
//! Playback: the clip plays a render of its original with the notes moved
//! (`faderframe_polypitch::render`), made on a worker once an edit has
//! settled (and no gesture is open). The render's source and the notes'
//! key go into the clip outside the undo history: undo and redo bring
//! back notes and audio together (they are one clip content), and where
//! they no longer agree the session renders again.

use crate::to_midi::harmony;
use crate::{NoticeLevel, Result, Session, SessionError};
use faderframe_core::{AudioSourceId, ClipId};
use faderframe_polypitch::{Heard, Moved, pitch_tracks};
use faderframe_project::pitch::{PitchEdit, PitchNote, Polyphonic, UNVOICED};
use faderframe_project::{AudioSource, ClipContent, Command, SourceSpec};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// How long notes stay unchanged before they are rendered.
const SETTLE: Duration = Duration::from_millis(300);
/// How often clips are looked over for renders to start.
const LOOK: Duration = Duration::from_millis(100);
/// Analysis step (seconds) and the shortest note kept.
const HOP_SECONDS: f64 = 0.005;
const SHORTEST: f64 = 0.06;

struct Render {
    clip: ClipId,
    key: u64,
    path: PathBuf,
    progress: Arc<AtomicU32>,
    handle: JoinHandle<std::result::Result<(u16, i64, u32), String>>,
}

#[derive(Default)]
pub(crate) struct PolyState {
    detect: Vec<(ClipId, JoinHandle<Option<PitchEdit>>)>,
    renders: Vec<Render>,
    /// Each polyphonic clip's notes' key as last seen and since when.
    seen: HashMap<ClipId, (u64, Instant)>,
    looked: Option<Instant>,
}

/// The notes of a polyphonic edit as the renderer moves them (source frames
/// at the source's `rate`; the edit's at the project's).
fn moved(e: &PitchEdit, rate: f64, project_rate: f64) -> Vec<Moved> {
    let scale = rate / project_rate.max(1.0);
    let hop = (f64::from(e.hop) * scale).round().max(1.0) as usize;
    e.notes
        .iter()
        .map(|n| {
            let frames = n.curve.len().max(1);
            let f0: Vec<f32> = (0..frames)
                .map(|k| match n.curve.get(k) {
                    Some(&c) if c != UNVOICED => {
                        faderframe_polypitch::hz(f64::from(n.pitch) + f64::from(c) / 100.0) as f32
                    }
                    Some(_) => 0.0,
                    None => faderframe_polypitch::hz(f64::from(n.pitch)) as f32,
                })
                .collect();
            let ratio: Vec<f32> = (0..frames)
                .map(|k| {
                    let at = n.start + k as i64 * i64::from(e.hop);
                    2f32.powf(n.correction_at(at, e.hop) / 12.0)
                })
                .collect();
            let gain = if n.muted {
                0.0
            } else {
                10f32.powf(n.gain_db / 20.0)
            };
            Moved {
                start: (n.start as f64 * scale).round() as i64,
                end: (n.end as f64 * scale).round() as i64,
                hop,
                f0,
                ratio,
                gain: vec![gain; frames],
            }
        })
        .collect()
}

/// Semitones above a note where its overtones (2nd–12th) fall.
const OVERTONES: [i32; 11] = [12, 19, 24, 28, 31, 34, 36, 38, 40, 42, 43];

/// Leave out notes that are another note's overtone: on one of its
/// overtones, sounding mostly with it, much quieter (the transcription
/// hears strong partials as notes now and then).
fn drop_overtones(found: &mut Vec<crate::to_midi::Found>) {
    let keep: Vec<bool> = found
        .iter()
        .map(|n| {
            !found.iter().any(|m| {
                let interval = i32::from(n.key) - i32::from(m.key);
                let overlap = n.end.min(m.end) - n.start.max(m.start);
                OVERTONES.contains(&interval)
                    && overlap >= 0.7 * (n.end - n.start)
                    && f32::from(n.velocity) < 0.6 * f32::from(m.velocity)
            })
        })
        .collect();
    let mut i = 0;
    found.retain(|_| {
        i += 1;
        keep[i - 1]
    });
}

/// The median of the finite values.
fn median(v: &[f32]) -> Option<f32> {
    let mut f: Vec<f32> = v.iter().copied().filter(|x| x.is_finite()).collect();
    if f.is_empty() {
        return None;
    }
    f.sort_by(f32::total_cmp);
    Some(f[f.len() / 2])
}

impl Session {
    /// Find every note of `clips` (polyphonic; on workers, placed when
    /// done).
    pub(crate) fn detect_polyphonic(&mut self, clips: &[ClipId]) -> Result<()> {
        let project_rate = f64::from(self.project.sample_rate.max(1));
        for &id in clips {
            let Some(ClipContent::Audio(a)) = self.project.clip(id).map(|c| &c.content) else {
                continue;
            };
            if a.spectral.is_some() || a.effects.is_some() {
                self.notify(
                    NoticeLevel::Warning,
                    "Polyphonic pitch editing works on clips without spectral edits and clip effects (Melodic works on them)",
                );
                continue;
            }
            if self.poly.detect.iter().any(|(c, _)| *c == id) {
                continue;
            }
            let original = a
                .pitch
                .as_ref()
                .and_then(|e| e.polyphonic)
                .map_or(a.source, |p| p.original);
            let Some(src) = self.sources.get(&original).cloned() else {
                continue;
            };
            let (offset, span) = (a.source_offset, a.source_span());
            let job = std::thread::Builder::new()
                .name("faderframe-polypitch".into())
                .spawn(move || {
                    let (mono, rate) = crate::pitch::mono_of(&src)?;
                    let scale = rate / project_rate;
                    let lo = ((offset as f64 * scale) as usize).min(mono.len());
                    let hi = (((offset + span) as f64 * scale) as usize).clamp(lo, mono.len());
                    let mut found = harmony(&mono[lo..hi], rate)?;
                    drop_overtones(&mut found);
                    let heard: Vec<Heard> = found
                        .iter()
                        .filter(|n| n.end - n.start >= SHORTEST)
                        .map(|n| Heard {
                            start: lo as i64 + (n.start * rate) as i64,
                            end: lo as i64 + (n.end * rate) as i64,
                            key: f32::from(n.key),
                        })
                        .collect();
                    let hop = ((HOP_SECONDS * rate).round() as usize).max(1);
                    let tracks = pitch_tracks(&mono, rate, &heard, hop);
                    // Into project-rate source frames.
                    let hop_p = ((hop as f64 / scale).round() as u32).max(1);
                    let mut notes: Vec<PitchNote> = heard
                        .iter()
                        .zip(tracks)
                        .filter_map(|(h, t)| {
                            let pitch = median(&t)?;
                            let curve = t
                                .iter()
                                .map(|v| {
                                    if v.is_finite() && (v - pitch).abs() < 3.0 {
                                        ((v - pitch) * 100.0).round() as i16
                                    } else {
                                        UNVOICED
                                    }
                                })
                                .collect();
                            Some(PitchNote {
                                start: (h.start as f64 / scale).round() as i64,
                                end: (h.end as f64 / scale).round() as i64,
                                pitch,
                                shift: 0.0,
                                drift: 0.0,
                                formant: 0.0,
                                curve,
                                gain_db: 0.0,
                                muted: false,
                            })
                        })
                        .collect();
                    notes.sort_by_key(|n| (n.start, (n.pitch * 100.0) as i64));
                    Some(PitchEdit {
                        hop: hop_p,
                        notes,
                        keep_formants: true,
                        polyphonic: Some(Polyphonic {
                            original,
                            rendered: 0,
                        }),
                    })
                })
                .map_err(|e| SessionError::Other(e.to_string()))?;
            self.poly.detect.push((id, job));
        }
        self.revision += 1;
        Ok(())
    }

    /// Polyphonic detections still running.
    pub fn detecting_polyphonic(&self) -> bool {
        !self.poly.detect.is_empty()
    }

    /// How far a clip's render is (0…1), while one runs.
    pub fn pitch_rendering(&self, clip: ClipId) -> Option<f32> {
        self.poly
            .renders
            .iter()
            .find(|r| r.clip == clip)
            .map(|r| f32::from_bits(r.progress.load(Ordering::Relaxed)))
    }

    /// Wait for detections and renders (tests, scripts).
    pub fn wait_for_polyphonic(&mut self) {
        let start = Instant::now();
        loop {
            self.poly.looked = None;
            for v in self.poly.seen.values_mut() {
                v.1 = v.1.checked_sub(SETTLE).unwrap_or(v.1);
            }
            self.poll_polyphonic();
            let pending = self.poly_pending();
            if (!pending && self.poly.detect.is_empty() && self.poly.renders.is_empty())
                || start.elapsed() > Duration::from_secs(180)
            {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// A polyphonic clip whose audio does not match its notes yet.
    fn poly_pending(&self) -> bool {
        self.project.clips.values().any(|c| {
            c.as_audio()
                .and_then(|a| a.pitch.as_ref())
                .is_some_and(|e| e.polyphonic.is_some_and(|p| p.rendered != e.sound_key()))
        })
    }

    /// Place finished detections and renders, start renders that are due
    /// (from the session tick).
    pub(crate) fn poll_polyphonic(&mut self) {
        // Detections.
        let mut i = 0;
        while i < self.poly.detect.len() {
            if !self.poly.detect[i].1.is_finished() {
                i += 1;
                continue;
            }
            let (clip, job) = self.poly.detect.swap_remove(i);
            match job.join().ok().flatten() {
                Some(edit) => {
                    if let Err(e) = self.place_detection(clip, edit) {
                        self.notify(NoticeLevel::Error, e.to_string());
                    }
                }
                None => self.notify(
                    NoticeLevel::Error,
                    "The notes of a clip's audio could not be found",
                ),
            }
            self.revision += 1;
        }
        // Renders.
        let mut i = 0;
        while i < self.poly.renders.len() {
            if !self.poly.renders[i].handle.is_finished() {
                i += 1;
                continue;
            }
            let r = self.poly.renders.swap_remove(i);
            match r.handle.join() {
                Ok(Ok(format)) => {
                    if let Err(e) = self.place_render(r.clip, r.key, &r.path, format) {
                        self.notify(NoticeLevel::Error, e.to_string());
                    }
                }
                Ok(Err(e)) => self.notify(NoticeLevel::Error, format!("pitch render: {e}")),
                Err(_) => self.notify(NoticeLevel::Error, "pitch render failed"),
            }
            self.revision += 1;
        }
        // Renders to start.
        if self.poly.looked.is_some_and(|t| t.elapsed() < LOOK) {
            return;
        }
        self.poly.looked = Some(Instant::now());
        let wanted: Vec<(ClipId, u64, Polyphonic)> = self
            .project
            .clips
            .values()
            .filter_map(|c| {
                let e = c.as_audio()?.pitch.as_ref()?;
                let p = e.polyphonic?;
                Some((c.id, e.sound_key(), p))
            })
            .collect();
        self.poly
            .seen
            .retain(|c, _| wanted.iter().any(|w| w.0 == *c));
        let gesture = self.history.in_gesture();
        for (clip, key, p) in wanted {
            let now = Instant::now();
            let seen = self.poly.seen.entry(clip).or_insert((key, now));
            if seen.0 != key {
                *seen = (key, now);
            }
            let settled = seen.1.elapsed() >= SETTLE;
            if key == p.rendered {
                continue;
            }
            if key == 0 {
                // Nothing moved: the original again.
                if let Err(e) = self.place_original(clip) {
                    self.notify(NoticeLevel::Error, e.to_string());
                }
                continue;
            }
            if !settled
                || gesture
                || self
                    .poly
                    .renders
                    .iter()
                    .any(|r| r.clip == clip && r.key == key)
            {
                continue;
            }
            if let Err(e) = self.start_render(clip, key, p.original) {
                self.notify(NoticeLevel::Error, e.to_string());
            }
        }
    }

    fn place_detection(&mut self, clip: ClipId, edit: PitchEdit) -> Result<()> {
        let Some(c) = self.project.clip(clip).cloned() else {
            return Ok(());
        };
        let Some(mut a) = c.as_audio().cloned() else {
            return Ok(());
        };
        let original = edit.polyphonic.map_or(a.source, |p| p.original);
        let count = edit.notes.len();
        a.source = original;
        a.pitch = Some(edit);
        self.batch(
            "Detect Pitch",
            vec![Command::SetClipContent {
                clip,
                start: c.start,
                content: Box::new(ClipContent::Audio(a)),
            }],
        )?;
        self.notify(
            NoticeLevel::Info,
            format!("Found {count} notes in ‘{}’ (polyphonic)", c.name),
        );
        Ok(())
    }

    fn start_render(&mut self, clip: ClipId, key: u64, original: AudioSourceId) -> Result<()> {
        let Some(c) = self.project.clip(clip) else {
            return Ok(());
        };
        let Some(e) = c.as_audio().and_then(|a| a.pitch.clone()) else {
            return Ok(());
        };
        let Some(data) = self.source_data(original) else {
            return Err(SessionError::Other("the clip's audio is not loaded".into()));
        };
        std::fs::create_dir_all(&self.media_dir).map_err(|e| SessionError::Other(e.to_string()))?;
        let path = crate::media::unique_path(&self.media_dir, &format!("{} Pitch", c.name));
        let project_rate = f64::from(self.project.sample_rate.max(1));
        let progress = Arc::new(AtomicU32::new(0));
        let (job_path, job_progress) = (path.clone(), Arc::clone(&progress));
        // (An older render of the clip still running is left to finish:
        // it no longer fits when it lands, and its file goes.)
        let handle = std::thread::Builder::new()
            .name("faderframe-pitch-render".into())
            .spawn(move || {
                let rate = data.rate();
                let channels = data.channels();
                let frames = data.frames();
                let mut audio = vec![vec![0.0f32; frames.max(0) as usize]; channels];
                let mut read = data.reader()?;
                read(0, &mut audio)?;
                drop(read);
                let notes = moved(&e, f64::from(rate), project_rate);
                let out = faderframe_polypitch::render(&audio, f64::from(rate), &notes, &mut |f| {
                    job_progress.store(f.to_bits(), Ordering::Relaxed);
                });
                let io = |e: std::io::Error| format!("{}: {e}", job_path.display());
                let mut w = faderframe_audio_files::wavstream::WavWriter::create(
                    &job_path,
                    channels as u16,
                    rate,
                    faderframe_audio_files::WavFormat::Float32,
                    false,
                )
                .map_err(io)?;
                let views: Vec<&[f32]> = out.iter().map(Vec::as_slice).collect();
                w.write_planar(&views, frames.max(0) as usize).map_err(io)?;
                w.finish().map_err(io)?;
                Ok((channels as u16, frames, rate))
            })
            .map_err(|e| SessionError::Other(e.to_string()))?;
        self.poly.renders.push(Render {
            clip,
            key,
            path,
            progress,
            handle,
        });
        self.revision += 1;
        Ok(())
    }

    /// Apply `commands` to the project without an undo step (a render
    /// that follows the notes: see the module docs).
    fn apply_unrecorded(&mut self, commands: Vec<Command>) -> Result<()> {
        let mut impact = faderframe_project::Impact::None;
        for cmd in commands {
            let i = cmd.impact();
            cmd.apply(&mut self.project)?;
            impact = impact.max(i);
        }
        self.sync(impact)
    }

    fn place_render(
        &mut self,
        clip: ClipId,
        key: u64,
        path: &std::path::Path,
        (channels, frames, rate): (u16, i64, u32),
    ) -> Result<()> {
        let current = self.project.clip(clip).cloned();
        let fits = current
            .as_ref()
            .and_then(|c| c.as_audio())
            .and_then(|a| a.pitch.as_ref())
            .is_some_and(|e| {
                e.sound_key() == key && e.polyphonic.is_some_and(|p| p.rendered != key)
            });
        let Some(c) = current.filter(|_| fits) else {
            // The notes moved on meanwhile (or the clip went).
            let _ = std::fs::remove_file(path);
            return Ok(());
        };
        let Some(mut a) = c.as_audio().cloned() else {
            return Ok(());
        };
        let source = AudioSource {
            id: self.project.ids.allocate(),
            name: path
                .file_stem()
                .map_or_else(|| c.name.clone(), |s| s.to_string_lossy().to_string()),
            spec: SourceSpec::File {
                path: path.to_path_buf(),
                channels,
                frames,
                sample_rate: rate,
            },
        };
        a.source = source.id;
        if let Some(p) = a.pitch.as_mut().and_then(|e| e.polyphonic.as_mut()) {
            p.rendered = key;
        }
        self.apply_unrecorded(vec![
            Command::AddSource {
                source: Box::new(source),
            },
            Command::SetClipContent {
                clip,
                start: c.start,
                content: Box::new(ClipContent::Audio(a)),
            },
        ])
    }

    /// Nothing moved any more: the clip plays its original again.
    fn place_original(&mut self, clip: ClipId) -> Result<()> {
        let Some(c) = self.project.clip(clip).cloned() else {
            return Ok(());
        };
        let Some(mut a) = c.as_audio().cloned() else {
            return Ok(());
        };
        let Some(p) = a.pitch.as_mut().and_then(|e| e.polyphonic.as_mut()) else {
            return Ok(());
        };
        p.rendered = 0;
        a.source = p.original;
        self.apply_unrecorded(vec![Command::SetClipContent {
            clip,
            start: c.start,
            content: Box::new(ClipContent::Audio(a)),
        }])
    }
}
