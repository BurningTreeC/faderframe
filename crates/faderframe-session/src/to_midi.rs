//! Audio to MIDI: an audio clip's notes on a new instrument track, as a
//! MIDI clip in the same place.
//!
//! Three ways to hear it, run in a thread on the clip's part of its
//! source: **Melody** (a single voice or instrument) by the pitch-editing
//! analysis ([`faderframe_analysis::melody`], velocities from the level);
//! **Harmony** (anything, chords too) by basic-pitch
//! ([`faderframe_transcribe`], on a 22 050 Hz copy); **Drums** by the
//! onsets, each hit sorted by where its energy sits into a kick (C1), a
//! snare (D1) or a closed hat (F#1), the Drum Sampler's pads. The track,
//! its instrument (the built-in synth, or the Drum Sampler for drums) and
//! the clip arrive in one undo step.

use crate::pitch::mono_of;
use crate::{NoticeLevel, Result, Session, SessionError};
use faderframe_core::{ClipId, TrackId};
use faderframe_project::{
    Clip, ClipContent, Command, MidiClip, MidiNote, PluginRef, PluginSlot, Track, TrackColor,
    TrackKind,
};
use faderframe_timeline::MusicalTime;
use std::thread::JoinHandle;

/// How a clip is heard as notes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToMidi {
    Melody,
    Harmony,
    Drums,
}

impl ToMidi {
    pub fn label(self) -> &'static str {
        match self {
            ToMidi::Melody => "Melody",
            ToMidi::Harmony => "Harmony",
            ToMidi::Drums => "Drums",
        }
    }
}

/// A note found: seconds into the clip's part of the source.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Found {
    pub(crate) start: f64,
    pub(crate) end: f64,
    pub(crate) key: u8,
    pub(crate) velocity: u8,
}

/// A conversion running: its clip, how, and the notes it will find.
type Job = (ClipId, ToMidi, JoinHandle<Option<Vec<Found>>>);

/// Conversions running.
#[derive(Default)]
pub(crate) struct Conversions {
    jobs: Vec<Job>,
}

/// The drum pads hits go to (the Drum Sampler's first pads from C1).
const KICK: u8 = 36;
const SNARE: u8 = 38;
const HAT: u8 = 42;

pub(crate) fn melody(x: &[f32], rate: f64) -> Vec<Found> {
    use faderframe_analysis::melody;
    let tr = melody::track(x, rate);
    let secs = |f: usize| (f * tr.hop) as f64 / rate;
    melody::notes(&tr)
        .into_iter()
        .map(|n| {
            let level = tr.frames[n.start..n.end]
                .iter()
                .map(|f| f.level_db)
                .fold(f32::MIN, f32::max);
            Found {
                start: secs(n.start),
                end: secs(n.end),
                key: n.pitch.round().clamp(0.0, 127.0) as u8,
                velocity: (27.0 + (level + 40.0).clamp(0.0, 40.0) * 2.5).round() as u8,
            }
        })
        .collect()
}

pub(crate) fn harmony(x: &[f32], rate: f64) -> Option<Vec<Found>> {
    let audio = resample(x, rate, faderframe_transcribe::RATE)?;
    let threads = std::thread::available_parallelism().map_or(2, |n| n.get());
    let notes = faderframe_transcribe::transcribe(
        &audio,
        &faderframe_transcribe::Settings::default(),
        threads,
    )
    .ok()?;
    Some(
        notes
            .into_iter()
            .map(|n| Found {
                start: n.start,
                end: n.end,
                key: n.key,
                velocity: (127.0 * n.amplitude).round().clamp(1.0, 127.0) as u8,
            })
            .collect(),
    )
}

/// `x` at `to` Hz.
pub(crate) fn resample(x: &[f32], from: f64, to: f64) -> Option<Vec<f32>> {
    if (from - to).abs() < 0.5 {
        return Some(x.to_vec());
    }
    let mut r =
        faderframe_audio_files::resample::StreamResampler::new(from as u32, to as u32, 1).ok()?;
    let mut out = Vec::with_capacity((x.len() as f64 * to / from) as usize + 1024);
    let mut sink = |planes: &[&[f32]], n: usize| {
        out.extend_from_slice(&planes[0][..n]);
        Ok(())
    };
    for chunk in x.chunks(1 << 16) {
        r.push(&[chunk.to_vec()], chunk.len(), &mut sink).ok()?;
    }
    r.finish(&mut sink).ok()?;
    Some(out)
}

/// A one-pole low pass's coefficient at `hz`.
fn coefficient(hz: f64, rate: f64) -> f32 {
    (1.0 - (-std::f64::consts::TAU * hz / rate).exp()) as f32
}

fn drums(x: &[f32], rate: f64) -> Vec<Found> {
    let span = (0.06 * rate) as usize;
    let peak_at = |at: usize| {
        x[at.min(x.len())..(at + span).min(x.len())]
            .iter()
            .fold(0.0f32, |m, v| m.max(v.abs()))
    };
    // Hits: onsets a little above the noise floor (a kick's low thump
    // adds less spectral flux than a snare's noise), and one at the very
    // start when the audio opens on a hit (no frame before it to differ
    // from).
    let mut hits: Vec<usize> = faderframe_audio_files::onsets::detect(x, rate as u32)
        .iter()
        .filter(|o| o.strength >= 0.08)
        .map(|o| o.frame.max(0) as usize)
        .collect();
    let loudest = x.iter().fold(0.0f32, |m, v| m.max(v.abs()));
    let opening = peak_at(0);
    if loudest > 0.0
        && opening > 0.25 * loudest
        && hits.first().is_none_or(|h| *h as f64 > 0.03 * rate)
    {
        let attack = x.iter().position(|v| v.abs() >= opening / 5.0).unwrap_or(0);
        hits.insert(0, attack);
    }
    let strongest = hits
        .iter()
        .map(|h| peak_at(*h))
        .fold(0.0f32, f32::max)
        .max(1e-9);
    let (cl, ch) = (coefficient(150.0, rate), coefficient(5_000.0, rate));
    hits.into_iter()
        .map(|at| {
            let at = at.min(x.len());
            let part = &x[at..(at + span).min(x.len())];
            // Energy under 150 Hz, over 5 kHz, and all of it.
            let (mut lo_s, mut hi_s) = (0.0f32, 0.0f32);
            let (mut e_low, mut e_high, mut e_all) = (0.0f64, 0.0f64, 0.0f64);
            for v in part {
                lo_s += cl * (v - lo_s);
                hi_s += ch * (v - hi_s);
                let high = v - hi_s;
                e_low += f64::from(lo_s * lo_s);
                e_high += f64::from(high * high);
                e_all += f64::from(v * v);
            }
            let all = e_all.max(1e-12);
            let key = if e_low / all > 0.45 {
                KICK
            } else if e_high / all > 0.35 {
                HAT
            } else {
                SNARE
            };
            let start = at as f64 / rate;
            Found {
                start,
                end: start + 0.1,
                key,
                velocity: (40.0 + 87.0 * (peak_at(at) / strongest)).round() as u8,
            }
        })
        .collect()
}

impl Session {
    /// Hear an audio clip's notes and put them on a new instrument track
    /// (see the module docs).
    pub fn convert_to_midi(&mut self, clip: ClipId, how: ToMidi) -> Result<()> {
        let c = self
            .project
            .clip(clip)
            .cloned()
            .ok_or_else(|| SessionError::Other(format!("no clip {clip}")))?;
        let Some(a) = c.as_audio() else {
            return Err(SessionError::Other(
                "only audio clips turn into MIDI".into(),
            ));
        };
        let source = self
            .sources
            .get(&a.source)
            .cloned()
            .ok_or_else(|| SessionError::Other("the clip's audio is missing".into()))?;
        let (from, span) = (a.source_offset, a.source_span());
        let project_rate = self.project.sample_rate as f64;
        let job = std::thread::Builder::new()
            .name("faderframe-to-midi".into())
            .spawn(move || {
                let (mono, rate) = mono_of(&source)?;
                let k = rate / project_rate;
                let a = ((from as f64 * k) as usize).min(mono.len());
                let b = (((from + span) as f64 * k) as usize).clamp(a, mono.len());
                let x = &mono[a..b];
                match how {
                    ToMidi::Melody => Some(melody(x, rate)),
                    ToMidi::Harmony => harmony(x, rate),
                    ToMidi::Drums => Some(drums(x, rate)),
                }
            })
            .map_err(|e| SessionError::Other(e.to_string()))?;
        self.conversions.jobs.push((clip, how, job));
        self.notify(
            NoticeLevel::Info,
            format!(
                "Listening for the {} in ‘{}’…",
                how.label().to_lowercase(),
                c.name
            ),
        );
        self.revision += 1;
        Ok(())
    }

    /// Conversions still running.
    pub fn converting_to_midi(&self) -> bool {
        !self.conversions.jobs.is_empty()
    }

    /// Place finished conversions (from the session tick).
    pub(crate) fn poll_conversions(&mut self) {
        let mut i = 0;
        while i < self.conversions.jobs.len() {
            if !self.conversions.jobs[i].2.is_finished() {
                i += 1;
                continue;
            }
            let (clip, how, job) = self.conversions.jobs.swap_remove(i);
            let result = match job.join().ok().flatten() {
                Some(found) => self.place_midi(clip, how, &found).map(|_| ()),
                None => Err(SessionError::Other(
                    "the clip's audio could not be read".into(),
                )),
            };
            if let Err(e) = result {
                self.notify(NoticeLevel::Warning, e.to_string());
            }
            self.revision += 1;
        }
    }

    /// The new track with its instrument and the notes as a clip.
    fn place_midi(&mut self, clip: ClipId, how: ToMidi, found: &[Found]) -> Result<TrackId> {
        let c = self
            .project
            .clip(clip)
            .cloned()
            .ok_or_else(|| SessionError::Other("the clip is gone".into()))?;
        let Some(a) = c.as_audio().cloned() else {
            return Err(SessionError::Other(
                "only audio clips turn into MIDI".into(),
            ));
        };
        if found.is_empty() {
            return Err(SessionError::Other(format!(
                "no {} heard in ‘{}’",
                if how == ToMidi::Drums {
                    "hits"
                } else {
                    "notes"
                },
                c.name
            )));
        }
        let p = &mut self.project;
        let rate = p.sample_rate as f64;
        let base = p.timeline.to_samples(c.start, rate);
        // Seconds into the clip's source part → the timeline, relative to
        // the clip.
        let at = |p: &faderframe_project::Project, s: f64| {
            let f = a.source_offset + (s * rate).round() as i64;
            let out = match &a.warp {
                Some(w) => w.output_of(a.source_offset, a.length, f),
                None => f - a.source_offset,
            };
            p.timeline.to_musical(base + out.clamp(0, a.length), rate) - c.start
        };
        let min = MusicalTime::from_quarters(1.0 / 32.0);
        let mut notes: Vec<MidiNote> = found
            .iter()
            .map(|n| {
                let start = at(p, n.start);
                MidiNote {
                    id: p.ids.allocate(),
                    start,
                    length: (at(p, n.end) - start).max(min),
                    key: n.key.min(127),
                    velocity: n.velocity.clamp(1, 127),
                    channel: 0,
                    muted: false,
                    release: None,
                }
            })
            .collect();
        notes.sort_by_key(|n| (n.start, n.key));
        let length = c.end(&p.timeline, p.sample_rate) - c.start;
        let track_id: TrackId = p.ids.allocate();
        let source_track = p.track(c.track).map(|t| t.color);
        let index = p
            .tracks
            .iter()
            .position(|t| t.id == c.track)
            .map_or(p.tracks.len(), |i| i + 1);
        let name = format!("{} {}", c.name, how.label());
        let track = Track::new(
            track_id,
            TrackKind::Instrument,
            name.clone(),
            source_track.unwrap_or_else(|| TrackColor::palette(p.tracks.len())),
        );
        let (id, label) = match how {
            ToMidi::Drums => (faderframe_core::builtin::DRUMS, "FaderFrame Drum Sampler"),
            _ => (faderframe_core::builtin::SYNTH, "FaderFrame Synth"),
        };
        let slot = PluginSlot {
            id: p.ids.allocate(),
            plugin: PluginRef::builtin(id, label),
            bypass: false,
            parameters: Vec::new(),
            state: None,
            sidechain: None,
        };
        let clip_id: ClipId = p.ids.allocate();
        let count = notes.len();
        let commands = vec![
            Command::AddTrack {
                track: Box::new(track),
                index,
            },
            Command::InsertPlugin {
                track: track_id,
                index: 0,
                slot,
            },
            Command::AddClip {
                clip: Box::new(Clip {
                    id: clip_id,
                    track: track_id,
                    name: name.clone(),
                    color: None,
                    start: c.start,
                    muted: false,
                    content: ClipContent::Midi(MidiClip {
                        length,
                        notes,
                        controllers: Vec::new(),
                        expressions: Vec::new(),
                        sysex: Vec::new(),
                    }),
                }),
            },
        ];
        self.batch("Convert to MIDI", commands)?;
        self.selection
            .select_tracks(&[track_id], crate::SelectMode::Replace);
        let more = if how == ToMidi::Drums {
            " — load sounds on the Drum Sampler's pads (kick C1, snare D1, hat F#1)"
        } else {
            ""
        };
        self.notify(NoticeLevel::Info, format!("‘{name}’: {count} notes{more}"));
        Ok(track_id)
    }
}
