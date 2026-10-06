//! Audio to MIDI: Spotify's basic-pitch (Apache-2.0; see `model/NOTICE`)
//! in plain Rust. Polyphonic: the notes of any instrument, chords
//! included.
//!
//! The model (a constant-Q transform, harmonic stacking and a few small
//! convolutions; 36 000 weights) runs on 2-second windows of 22 050 Hz
//! mono audio overlapping by 30 frames, through [`graph::Graph`], an
//! evaluator for the operators its converted graph uses; windows run on
//! the machine's cores. Its frame (note), onset and contour activations
//! are unwrapped as basic-pitch's `inference.py` does and turned into
//! notes as its `note_creation.py` does: onset peaks over a threshold
//! (and onsets inferred from rising frames) start notes that last while
//! the frames stay over theirs; the "melodia trick" then finds notes in
//! what energy is left. Velocity is the mean activation.

#![forbid(unsafe_code)]

pub mod graph;

use graph::{Graph, GraphError, Tensor, Value};

/// The model's sample rate.
pub const RATE: f64 = 22_050.0;
const FFT_HOP: usize = 256;
/// Samples a window takes.
const WINDOW: usize = 22_050 * 2 - FFT_HOP;
/// Frames a window gives.
const WINDOW_FRAMES: usize = 172;
const OVERLAP_FRAMES: usize = 30;
const MIDI_OFFSET: u8 = 21;
const KEYS: usize = 88;
/// basic-pitch's per-window timing correction (`model_frames_to_time`).
const MAGIC_ALIGNMENT_OFFSET: f64 = 0.0018;

static MODEL: &[u8] = include_bytes!("../model/basic-pitch.ffnn");

/// How notes are made from the activations.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    /// Least onset activation that starts a note (0…1).
    pub onset_threshold: f32,
    /// Least frame activation that keeps a note on (0…1).
    pub frame_threshold: f32,
    /// Shortest note (seconds).
    pub min_note_seconds: f64,
    /// Lowest and highest notes (MIDI).
    pub lowest: u8,
    pub highest: u8,
    /// Find notes in what is left after the onsets' notes.
    pub melodia_trick: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            onset_threshold: 0.5,
            frame_threshold: 0.3,
            min_note_seconds: 0.1277,
            lowest: MIDI_OFFSET,
            highest: MIDI_OFFSET + KEYS as u8 - 1,
            melodia_trick: true,
        }
    }
}

/// A transcribed note.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Note {
    /// Seconds from the start.
    pub start: f64,
    pub end: f64,
    pub key: u8,
    /// Mean activation (0…1), for the velocity.
    pub amplitude: f32,
}

/// The model's activations for a whole recording (frames × keys; contour
/// frames × 264).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Activations {
    pub frames: usize,
    pub note: Vec<f32>,
    pub onset: Vec<f32>,
    pub contour: Vec<f32>,
}

#[derive(Debug, thiserror::Error)]
pub enum TranscribeError {
    #[error("the transcription model: {0}")]
    Model(#[from] GraphError),
}

/// The loaded model.
pub struct Model {
    graph: Graph,
}

impl Model {
    pub fn load() -> Result<Self, TranscribeError> {
        Ok(Self {
            graph: Graph::parse(MODEL)?,
        })
    }

    /// The activations of one window (`WINDOW` samples): note, onset,
    /// contour (each frames × bins).
    pub fn window(&self, audio: &[f32]) -> Result<[Vec<f32>; 3], TranscribeError> {
        let mut x = vec![0.0f32; WINDOW];
        let n = audio.len().min(WINDOW);
        x[..n].copy_from_slice(&audio[..n]);
        let mut out = self.graph.run(Tensor::new(vec![1, WINDOW, 1], x))?;
        // The graph's outputs by name: …:0 contour, …:1 note, …:2 onset.
        let mut take = |suffix: &str| -> Result<Vec<f32>, TranscribeError> {
            let name = self
                .graph
                .outputs()
                .iter()
                .find(|o| o.ends_with(suffix))
                .cloned()
                .unwrap_or_default();
            match out.remove(&name) {
                Some(Value::F(t)) => Ok(t.data),
                _ => Err(TranscribeError::Model(GraphError::Missing(suffix.into()))),
            }
        };
        let contour = take(":0")?;
        let note = take(":1")?;
        let onset = take(":2")?;
        Ok([note, onset, contour])
    }

    /// The activations of mono `audio` at [`RATE`], unwrapped as
    /// basic-pitch does (windows on `threads` threads).
    pub fn activations(
        &self,
        audio: &[f32],
        threads: usize,
    ) -> Result<Activations, TranscribeError> {
        let overlap = OVERLAP_FRAMES * FFT_HOP;
        let hop = WINDOW - overlap;
        // Half an overlap of silence in front, then windows a hop apart.
        let mut padded = vec![0.0f32; overlap / 2];
        padded.extend_from_slice(audio);
        let starts: Vec<usize> = (0..padded.len().max(1)).step_by(hop).collect();
        let threads = threads.clamp(1, starts.len().max(1));
        let mut results: Vec<Option<Result<[Vec<f32>; 3], TranscribeError>>> =
            (0..starts.len()).map(|_| None).collect();
        std::thread::scope(|scope| {
            let chunks: Vec<_> = results
                .chunks_mut(starts.len().div_ceil(threads))
                .zip(starts.chunks(starts.len().div_ceil(threads)))
                .collect();
            for (out, st) in chunks {
                let padded = &padded;
                scope.spawn(move || {
                    for (o, s) in out.iter_mut().zip(st) {
                        let end = (s + WINDOW).min(padded.len());
                        *o = Some(self.window(&padded[*s..end]));
                    }
                });
            }
        });
        let olap = OVERLAP_FRAMES / 2;
        let kept = WINDOW_FRAMES - 2 * olap;
        let expected = (audio.len() as f64 / hop as f64 * kept as f64) as usize;
        let mut act = Activations::default();
        for r in results {
            let [note, onset, contour] = r.unwrap_or_else(|| Ok(Default::default()))?;
            for (dst, src, bins) in [
                (&mut act.note, &note, KEYS),
                (&mut act.onset, &onset, KEYS),
                (&mut act.contour, &contour, KEYS * 3),
            ] {
                if src.len() >= WINDOW_FRAMES * bins {
                    dst.extend_from_slice(&src[olap * bins..(olap + kept) * bins]);
                }
            }
        }
        act.frames = (act.note.len() / KEYS).min(expected);
        act.note.truncate(act.frames * KEYS);
        act.onset.truncate(act.frames * KEYS);
        act.contour.truncate(act.frames * KEYS * 3);
        Ok(act)
    }
}

/// The time of frame `f` (basic-pitch's `model_frames_to_time`).
pub fn frame_time(f: usize) -> f64 {
    let annot_frames = (RATE as usize / FFT_HOP * 2) as f64;
    let window_offset = (FFT_HOP as f64 / RATE) * (annot_frames - WINDOW as f64 / FFT_HOP as f64)
        + MAGIC_ALIGNMENT_OFFSET;
    f as f64 * FFT_HOP as f64 / RATE - window_offset * (f as f64 / annot_frames).floor()
}

/// The notes in `act` (basic-pitch's `output_to_notes_polyphonic`).
pub fn notes(act: &Activations, s: &Settings) -> Vec<Note> {
    let n = act.frames;
    if n < 3 {
        return Vec::new();
    }
    let at = |t: usize, k: usize| t * KEYS + k;
    let (lo, hi) = (
        usize::from(s.lowest.saturating_sub(MIDI_OFFSET)).min(KEYS),
        (usize::from(s.highest.saturating_sub(MIDI_OFFSET)) + 1).min(KEYS),
    );
    let mut frames = act.note.clone();
    let mut onsets = act.onset.clone();
    for t in 0..n {
        for k in (0..lo).chain(hi..KEYS) {
            frames[at(t, k)] = 0.0;
            onsets[at(t, k)] = 0.0;
        }
    }
    // Onsets inferred from frames rising (over 1 and 2 frames), scaled to
    // the onsets' maximum; the larger of the two.
    let max_onset = onsets.iter().copied().fold(0.0f32, f32::max);
    let mut diff = vec![0.0f32; n * KEYS];
    for t in 2..n {
        for k in 0..KEYS {
            let d1 = frames[at(t, k)] - frames[at(t - 1, k)];
            let d2 = frames[at(t, k)] - frames[at(t - 2, k)];
            diff[at(t, k)] = d1.min(d2).max(0.0);
        }
    }
    let max_diff = diff.iter().copied().fold(0.0f32, f32::max);
    if max_diff > 0.0 {
        for (o, d) in onsets.iter_mut().zip(&diff) {
            *o = o.max(max_onset * d / max_diff);
        }
    }
    let min_len = (s.min_note_seconds * RATE / FFT_HOP as f64).round() as usize;
    let tol = 11;
    let frame_thresh = s.frame_threshold;
    // Onset peaks (local maxima in time) over the threshold, latest first.
    let mut starts: Vec<(usize, usize)> = Vec::new();
    for t in 1..n - 1 {
        for k in 0..KEYS {
            let v = onsets[at(t, k)];
            if v >= s.onset_threshold && v > onsets[at(t - 1, k)] && v > onsets[at(t + 1, k)] {
                starts.push((t, k));
            }
        }
    }
    starts.sort_by(|a, b| b.cmp(a));
    let mut energy = frames.clone();
    let mut out: Vec<(usize, usize, usize, f32)> = Vec::new();
    let mean = |a: usize, b: usize, k: usize| {
        if b <= a {
            return 0.0;
        }
        (a..b).map(|t| frames[at(t, k)]).sum::<f32>() / (b - a) as f32
    };
    for (start, k) in starts {
        if start >= n - 1 {
            continue;
        }
        let mut i = start + 1;
        let mut quiet = 0;
        while i < n - 1 && quiet < tol {
            if energy[at(i, k)] < frame_thresh {
                quiet += 1;
            } else {
                quiet = 0;
            }
            i += 1;
        }
        i -= quiet;
        if i - start <= min_len {
            continue;
        }
        for t in start..i {
            energy[at(t, k)] = 0.0;
            if k + 1 < KEYS {
                energy[at(t, k + 1)] = 0.0;
            }
            if k > 0 {
                energy[at(t, k - 1)] = 0.0;
            }
        }
        out.push((start, i, k, mean(start, i, k)));
    }
    if s.melodia_trick {
        // The first largest (numpy's argmax), while over the threshold.
        let first_largest = |energy: &[f32]| {
            energy
                .iter()
                .enumerate()
                .fold(None, |best: Option<(usize, f32)>, (i, v)| match best {
                    Some((_, b)) if *v <= b => best,
                    _ => Some((i, *v)),
                })
                .filter(|(_, v)| *v > frame_thresh)
        };
        while let Some((idx, _)) = first_largest(&energy) {
            let (mid, k) = (idx / KEYS, idx % KEYS);
            energy[idx] = 0.0;
            let clear = |energy: &mut Vec<f32>, t: usize| {
                energy[at(t, k)] = 0.0;
                if k + 1 < KEYS {
                    energy[at(t, k + 1)] = 0.0;
                }
                if k > 0 {
                    energy[at(t, k - 1)] = 0.0;
                }
            };
            // Forward.
            let mut i = mid + 1;
            let mut quiet = 0;
            while i < n - 1 && quiet < tol {
                if energy[at(i, k)] < frame_thresh {
                    quiet += 1;
                } else {
                    quiet = 0;
                }
                clear(&mut energy, i);
                i += 1;
            }
            let end = i - 1 - quiet;
            // Backward.
            let mut i = mid as i64 - 1;
            let mut quiet = 0;
            while i > 0 && quiet < tol {
                if energy[at(i as usize, k)] < frame_thresh {
                    quiet += 1;
                } else {
                    quiet = 0;
                }
                clear(&mut energy, i as usize);
                i -= 1;
            }
            let begin = (i + 1 + quiet as i64).max(0) as usize;
            if end <= begin || end - begin <= min_len {
                continue;
            }
            out.push((begin, end, k, mean(begin, end, k)));
        }
    }
    let mut notes: Vec<Note> = out
        .into_iter()
        .map(|(a, b, k, amp)| Note {
            start: frame_time(a),
            end: frame_time(b),
            key: k as u8 + MIDI_OFFSET,
            amplitude: amp,
        })
        .collect();
    notes.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.key.cmp(&b.key)));
    notes
}

/// Transcribe mono `audio` at [`RATE`] (see the module docs).
pub fn transcribe(
    audio: &[f32],
    settings: &Settings,
    threads: usize,
) -> Result<Vec<Note>, TranscribeError> {
    let model = Model::load()?;
    let act = model.activations(audio, threads)?;
    Ok(notes(&act, settings))
}
