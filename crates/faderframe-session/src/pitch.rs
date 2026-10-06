//! Pitch editing in the session: finding the notes of audio clips and
//! editing them.
//!
//! Detection ([`faderframe_analysis::melody`]) runs in a thread per source
//! (its mono mix, read from the media file or the in-memory data) and is
//! kept for the session; [`Session::detect_pitch`] gives each clip a
//! [`PitchEdit`] of the notes inside it, in one undo step, once its
//! source is analysed. Edits ([`PitchOp`]) change the edit's notes;
//! drags send the total move inside one gesture and recompute from the
//! clip as the gesture found it ([`Session::gesture_clip`]).

use crate::{Result, Session, SessionError};
use faderframe_analysis::melody::{self, PitchTrack};
use faderframe_audio_files::onsets;
use faderframe_core::{AudioSourceId, ClipId};
use faderframe_engine::Source;
use faderframe_project::pitch::{self, MAX_FORMANT, MAX_SHIFT, PitchEdit, PitchNote, UNVOICED};
use faderframe_project::{AudioClip, Clip, ClipContent};
use faderframe_timeline::MusicalTime;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::thread::JoinHandle;

/// An edit of a clip's notes (`notes` empty: all of them).
#[derive(Clone, Debug, PartialEq)]
pub enum PitchOp {
    /// Move notes by semitones (inside a gesture: from where it began).
    Move { notes: Vec<usize>, by: f32 },
    /// Set notes' corrections.
    Set {
        notes: Vec<usize>,
        shift: Option<f32>,
        drift: Option<f32>,
        formant: Option<f32>,
    },
    /// Bring notes `amount` (0…1) of the way to the nearest note of the
    /// key where they play (semitones without one), straightened by
    /// `drift`.
    Correct {
        notes: Vec<usize>,
        amount: f32,
        drift: f32,
    },
    /// Split a note at a source frame.
    Split { note: usize, at: i64 },
    /// Join neighbouring notes (the first through the last listed).
    Join { notes: Vec<usize> },
    /// Back to as sung.
    Reset { notes: Vec<usize> },
    /// Keep the formants where the pitch moves.
    KeepFormants(bool),
    /// Forget the analysis and its edits.
    Remove,
}

/// A source's pitch track and its sample rate (`None`: unreadable).
type Analysis = Option<(PitchTrack, f64)>;

/// Analysed sources and the analyses running.
#[derive(Default)]
pub(crate) struct PitchCache {
    /// A source's pitch track and its sample rate.
    tracks: HashMap<AudioSourceId, (Arc<PitchTrack>, f64)>,
    jobs: Vec<(AudioSourceId, JoinHandle<Analysis>)>,
    /// Clips waiting for their source's analysis.
    waiting: Vec<ClipId>,
}

/// A media file's mono mix and rate.
fn read_mono(media: &Path) -> std::io::Result<(Vec<f32>, f64)> {
    let f = faderframe_audio_files::wavstream::WavFile::open(media)?;
    let chunk = 1 << 16;
    let mut mono = Vec::with_capacity(f.frames() as usize);
    let mut bufs = vec![vec![0.0f32; chunk]; f.channels()];
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
    Ok((mono, f64::from(f.sample_rate())))
}

/// A source's mono mix and rate (helper threads: reads the media file
/// of a streamed source).
pub(crate) fn mono_of(source: &Source) -> Option<(Vec<f32>, f64)> {
    match source {
        Source::Memory(d) => {
            let views: Vec<&[f32]> = (0..d.num_channels()).map(|c| d.channel(c)).collect();
            Some((onsets::mixdown(&views), f64::from(d.sample_rate())))
        }
        Source::Stream(st) => read_mono(st.path()).ok(),
    }
}

/// The analysis of a source (helper thread).
fn analyse(source: Source) -> Analysis {
    let (mono, rate) = mono_of(&source)?;
    Some((melody::track(&mono, rate), rate))
}

/// The note of `track` at fractional frame `f`, `None` where unvoiced.
fn track_at(track: &PitchTrack, f: f64) -> Option<f32> {
    let i = f.floor().max(0.0) as usize;
    let a = track.frames.get(i).filter(|x| x.voiced()).map(|x| x.note);
    let b = track
        .frames
        .get(i + 1)
        .filter(|x| x.voiced())
        .map(|x| x.note);
    match (a, b) {
        (Some(a), Some(b)) => Some(a + (b - a) * (f - i as f64) as f32),
        (a, b) => a.or(b),
    }
}

/// Furthest a note's curve strays from its pitch (semitones; a scoop into
/// it, not the tracker an octave off or the next note's onset).
const CURVE_REACH: f32 = 5.0;

/// The pitch edit of clip `a` from its source's track (`source_rate`),
/// in a project at `project_rate`: the notes inside the clip.
pub fn edit_from(
    track: &PitchTrack,
    source_rate: f64,
    project_rate: f64,
    a: &AudioClip,
) -> PitchEdit {
    // Project frames a source frame.
    let scale = project_rate / source_rate.max(1.0);
    let hop = ((track.hop as f64 * scale).round() as u32).max(1);
    let (lo, hi) = (a.source_offset, a.source_offset + a.source_span());
    let frame = |f: usize| (f as f64 * track.hop as f64 * scale).round() as i64;
    let notes = melody::notes(track)
        .into_iter()
        .filter_map(|n| {
            let (start, end) = (frame(n.start), frame(n.end));
            if end <= lo || start >= hi {
                return None;
            }
            let count = ((end - start) / i64::from(hop)).max(1) as usize;
            let curve = (0..count)
                .map(|k| {
                    let at = start + k as i64 * i64::from(hop);
                    let f = at as f64 / scale / track.hop as f64;
                    match track_at(track, f) {
                        Some(m) if (m - n.pitch).abs() <= CURVE_REACH => {
                            ((m - n.pitch) * 100.0).round() as i16
                        }
                        _ => UNVOICED,
                    }
                })
                .collect();
            Some(PitchNote {
                start,
                end,
                pitch: n.pitch,
                shift: 0.0,
                drift: 0.0,
                formant: 0.0,
                curve,
            })
        })
        .collect();
    PitchEdit {
        hop,
        notes,
        keep_formants: true,
    }
}

fn audio_of(c: &Clip) -> Result<AudioClip> {
    match &c.content {
        ClipContent::Audio(a) => Ok(a.clone()),
        _ => Err(SessionError::Other(
            "pitch editing works on audio clips".into(),
        )),
    }
}

/// `notes` (all of `count` when empty), in range, sorted, once each.
fn chosen(notes: &[usize], count: usize) -> Vec<usize> {
    let mut v: Vec<usize> = if notes.is_empty() {
        (0..count).collect()
    } else {
        notes.iter().copied().filter(|i| *i < count).collect()
    };
    v.sort_unstable();
    v.dedup();
    v
}

impl Session {
    /// Find the notes of `clips` (audio clips; at once when their sources
    /// are analysed, else when the analyses finish).
    pub fn detect_pitch(&mut self, clips: &[ClipId]) -> Result<()> {
        let mut ready = Vec::new();
        for &id in clips {
            let Some(ClipContent::Audio(a)) = self.project.clip(id).map(|c| &c.content) else {
                continue;
            };
            let source = a.source;
            if self.pitch.tracks.contains_key(&source) {
                ready.push(id);
                continue;
            }
            if !self.pitch.waiting.contains(&id) {
                self.pitch.waiting.push(id);
            }
            if self.pitch.jobs.iter().any(|(s, _)| *s == source) {
                continue;
            }
            let Some(src) = self.sources.get(&source).cloned() else {
                continue;
            };
            let job = std::thread::Builder::new()
                .name("faderframe-pitch".into())
                .spawn(move || analyse(src))
                .map_err(|e| SessionError::Other(e.to_string()))?;
            self.pitch.jobs.push((source, job));
        }
        self.revision += 1;
        self.apply_detection(&ready)
    }

    /// Pitch analyses still running.
    pub fn detecting_pitch(&self) -> bool {
        !self.pitch.jobs.is_empty()
    }

    /// Collect finished analyses and give the clips waiting for them
    /// their notes (from the session tick).
    pub(crate) fn poll_pitch(&mut self) {
        let mut done = false;
        let mut i = 0;
        while i < self.pitch.jobs.len() {
            if !self.pitch.jobs[i].1.is_finished() {
                i += 1;
                continue;
            }
            let (source, job) = self.pitch.jobs.swap_remove(i);
            match job.join().ok().flatten() {
                Some((track, rate)) => {
                    self.pitch.tracks.insert(source, (Arc::new(track), rate));
                }
                None => self.notify(
                    crate::NoticeLevel::Error,
                    "The pitch of a clip's audio could not be read",
                ),
            }
            done = true;
        }
        if !done {
            return;
        }
        let ready: Vec<ClipId> = self
            .pitch
            .waiting
            .iter()
            .copied()
            .filter(|c| match self.project.clip(*c).map(|c| &c.content) {
                Some(ClipContent::Audio(a)) => !self.pitch.jobs.iter().any(|(s, _)| *s == a.source),
                _ => true,
            })
            .collect();
        self.pitch.waiting.retain(|c| !ready.contains(c));
        if let Err(e) = self.apply_detection(&ready) {
            self.notify(crate::NoticeLevel::Error, e.to_string());
        }
        self.revision += 1;
    }

    /// Give `clips` the notes their (analysed) sources hold.
    fn apply_detection(&mut self, clips: &[ClipId]) -> Result<()> {
        let rate = self.project.sample_rate as f64;
        let mut commands = Vec::new();
        for &id in clips {
            let Some(c) = self.project.clip(id) else {
                continue;
            };
            let Ok(mut a) = audio_of(c) else {
                continue;
            };
            let Some((track, source_rate)) = self.pitch.tracks.get(&a.source) else {
                continue;
            };
            a.pitch = Some(edit_from(track, *source_rate, rate, &a));
            commands.push(faderframe_project::Command::SetClipContent {
                clip: id,
                start: c.start,
                content: Box::new(ClipContent::Audio(a)),
            });
        }
        if commands.is_empty() {
            return Ok(());
        }
        self.batch("Detect Pitch", commands)
    }

    /// Where source frame `at` of clip `c` plays on the timeline.
    fn pitch_time(&self, c: &Clip, a: &AudioClip, at: i64) -> MusicalTime {
        let p = &self.project;
        let rate = p.sample_rate as f64;
        let out = match &a.warp {
            Some(w) => w.output_of(a.source_offset, a.length, at),
            None => at - a.source_offset,
        };
        let base = p.timeline.to_samples(c.start, rate);
        p.timeline.to_musical(base + out, rate)
    }

    /// The scale notes snap to at `t`: (root, steps) of the key there.
    fn scale_at(&self, t: MusicalTime) -> Option<(u8, u16)> {
        let key = self.project.key_at(t)?;
        let steps = key
            .scale
            .intervals()
            .iter()
            .fold(0u16, |m, i| m | 1 << (i % 12));
        Some((key.root % 12, steps))
    }

    /// Edit the notes of a clip's pitch edit.
    pub fn edit_pitch(&mut self, clip: ClipId, op: PitchOp) -> Result<()> {
        let c = self.gesture_clip(clip)?;
        let mut a = audio_of(&c)?;
        let Some(mut e) = a.pitch.take() else {
            return Err(SessionError::Other(
                "detect the clip's pitch before editing it".into(),
            ));
        };
        let count = e.notes.len();
        let label = match op {
            PitchOp::Move { notes, by } => {
                for i in chosen(&notes, count) {
                    let n = &mut e.notes[i];
                    n.shift = (n.shift + by).clamp(-MAX_SHIFT, MAX_SHIFT);
                }
                "Move Notes"
            }
            PitchOp::Set {
                notes,
                shift,
                drift,
                formant,
            } => {
                for i in chosen(&notes, count) {
                    let n = &mut e.notes[i];
                    if let Some(s) = shift {
                        n.shift = s.clamp(-MAX_SHIFT, MAX_SHIFT);
                    }
                    if let Some(d) = drift {
                        n.drift = d.clamp(0.0, 1.0);
                    }
                    if let Some(f) = formant {
                        n.formant = f.clamp(-MAX_FORMANT, MAX_FORMANT);
                    }
                }
                "Edit Notes"
            }
            PitchOp::Correct {
                notes,
                amount,
                drift,
            } => {
                for i in chosen(&notes, count) {
                    let t = self.pitch_time(&c, &a, e.notes[i].start);
                    let scale = self.scale_at(t);
                    let n = &mut e.notes[i];
                    let heard = n.heard();
                    n.shift = (n.shift
                        + amount.clamp(0.0, 1.0) * (pitch::snap(heard, scale) - heard))
                        .clamp(-MAX_SHIFT, MAX_SHIFT);
                    n.drift = drift.clamp(0.0, 1.0);
                }
                "Correct Pitch"
            }
            PitchOp::Split { note, at } => {
                let Some((x, y)) = e.notes.get(note).and_then(|n| pitch::split(n, at, e.hop))
                else {
                    return Ok(());
                };
                e.notes.splice(note..=note, [x, y]);
                "Split Note"
            }
            PitchOp::Join { notes } => {
                let v = chosen(&notes, count);
                let (Some(&first), Some(&last)) = (v.first(), v.last()) else {
                    return Ok(());
                };
                if first == last {
                    return Ok(());
                }
                let Some(joined) = pitch::join(&e.notes[first..=last], e.hop) else {
                    return Ok(());
                };
                e.notes.splice(first..=last, [joined]);
                "Join Notes"
            }
            PitchOp::Reset { notes } => {
                for i in chosen(&notes, count) {
                    let n = &mut e.notes[i];
                    (n.shift, n.drift, n.formant) = (0.0, 0.0, 0.0);
                }
                "Reset Pitch"
            }
            PitchOp::KeepFormants(keep) => {
                e.keep_formants = keep;
                "Formants"
            }
            PitchOp::Remove => {
                return self.set_audio("Remove Pitch Edit", &c, c.start, a);
            }
        };
        a.pitch = Some(e);
        self.set_audio(label, &c, c.start, a)
    }

    /// The pitch track of a clip's source, once analysed (for display).
    pub fn pitch_track(&self, source: AudioSourceId) -> Option<Arc<PitchTrack>> {
        self.pitch.tracks.get(&source).map(|(t, _)| Arc::clone(t))
    }
}
